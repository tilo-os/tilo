//! Input routing for [`ChatApp`]. Mouse presses hit-test the regions
//! recorded during the previous `draw`; keyboard input drives the focused
//! composer, splicing directly into the per-channel draft `Text` object in
//! the CRDT (so a draft is collaborative state, and undo/redo come for
//! free from the toolkit's snapshotting).

#[cfg(test)]
use automerge::AutoCommit;
use taffy::{NodeId, Style, TaffyTree};
use vello::kurbo::Point;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use tilo_rich_text::FieldOutcome;
use ui::Widget;

use crate::app::{
    AttachmentImportPending, AttachmentImportResult, AttachmentPickerResult, ChatApp, Focus, Hit,
    MenuAction, MenuTarget, RowSwipe,
};
use crate::messages::chat_part;
use crate::model::{ChannelKind, Id};
use crate::parts::ChatPart;
use crate::record::chat;
use crate::resource::Chat;

/// A new item's id, minted where it is made: a channel or category the
/// window adds and renames at once.
pub(crate) fn fresh_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

/// The record's id for `id`.
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

/// Feed one raw winit event into an embedded [`TextInput`]. The composers live
/// outside the taffy tree (the app lays them out by hand), so a throwaway leaf
/// node satisfies the `Widget::handle_event` signature; the input only uses it
/// to report a focus target, which the composer host maps to its retained owner.
pub(crate) fn drive_text_input(
    ti: &mut impl Widget,
    event: &WindowEvent,
    scale: f64,
    modifiers: Modifiers,
    doc: &mut ui::widget::Edits<'_>,
) -> bool {
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let Ok(node) = taffy.new_leaf(Style::default()) else {
        return false;
    };
    let mut handled = false;
    let mut focus_target: Option<NodeId> = None;
    ti.handle_event(
        event,
        &mut ui::widget::EventCtx {
            scale_factor: scale,
            modifiers: &modifiers,
            state: doc.reborrow(),
            path: &automerge::ROOT,
            handled: &mut handled,
            focus_target: &mut focus_target,
            node_id: node,
            taffy: &taffy,
        },
    );
    focus_target.is_some()
}

/// Drive a retained shared control from Chat's immediate-mode event host.
/// The temporary layout node only satisfies `Widget::handle_event`; menu
/// geometry is established synchronously by its owner before dispatch.
fn drive_retained_control(
    menu: &mut dyn ui::Widget,
    event: &WindowEvent,
    scale: f64,
    modifiers: Modifiers,
    doc: &mut ui::widget::Edits<'_>,
) -> bool {
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let Ok(node) = taffy.new_leaf(Style::default()) else {
        return false;
    };
    let mut handled = false;
    let mut focus_target = None;
    menu.handle_event(
        event,
        &mut ui::widget::EventCtx {
            scale_factor: scale,
            modifiers: &modifiers,
            state: doc.reborrow(),
            path: &automerge::ROOT,
            handled: &mut handled,
            focus_target: &mut focus_target,
            node_id: node,
            taffy: &taffy,
        },
    );
    handled
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

    /// Scroll what is under the pointer: the emoji grid, an open thread
    /// (which keeps the wheel from the timeline behind it), or the timeline.
    fn handle_wheel(&mut self, delta: &MouseScrollDelta, scale: f64) {
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * 44.0,
            MouseScrollDelta::PixelDelta(p) => p.y / scale,
        };
        if self.emoji_open && self.emoji_grid.contains(self.last_mouse) {
            // Clamped against the content height when painted.
            self.emoji_scroll = (self.emoji_scroll - dy).max(0.0);
            return;
        }
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
        if self.route_thread_scroll_event(event, scale, modifiers, doc) {
            *handled = true;
            return false;
        }
        if matches!(
            event,
            WindowEvent::MouseWheel { .. }
                | WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    ..
                }
                | WindowEvent::KeyboardInput {
                    event: KeyEvent {
                        state: ElementState::Pressed,
                        ..
                    },
                    ..
                }
        ) {
            let _ = self.tooltips.dismiss();
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let last_mouse = Point::new(position.x / scale, position.y / scale);
                self.handle_cursor_moved(
                    last_mouse,
                    EventDispatch::new(event, scale, modifiers, doc, handled),
                );
            }
            // A file dropped on the conversation is shared there.
            WindowEvent::DroppedFile(path) => {
                if self.can_participate() {
                    self.begin_attachment_import(path);
                }
                *handled = true;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_wheel(delta, scale);
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
                self.handle_left_release(event, scale, modifiers, doc, handled);
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

    /// A pointer move: live-drags the floating call overlay or an in-progress
    /// channel-row swipe, then forwards hover state to both composers.
    fn handle_cursor_moved(&mut self, last_mouse: Point, dispatch: EventDispatch<'_, '_>) {
        let EventDispatch {
            event,
            scale,
            modifiers,
            doc,
            handled,
        } = dispatch;
        self.last_mouse = last_mouse;
        let tooltip_target = self.hover_tooltip_target(last_mouse);
        if self.tooltips.on_pointer(tooltip_target, ui::time::now()) {
            *handled = true;
        }
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
                    Hit::CallParticipantsToggle | Hit::CallMute | Hit::CallExpand | Hit::CallLeave
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
        // Track an in-progress channel-row swipe.
        if let Some(sw) = &mut self.swipe {
            sw.current = self.last_mouse;
            *handled = true;
        }
        // Keep every shared input's hover/drag state current so clicks place
        // carets and press-drags extend selections on inline editors too.
        self.forward_pointer_to_text_inputs(event, scale, modifiers, doc);
    }

    /// A left-button press: begins a call-overlay drag, arms a channel-row
    /// swipe, or resolves an ordinary hit (forwarding the press into a
    /// composer that was just focused by it).
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
                let text_input_hit = matches!(
                    hit,
                    Hit::FocusMain
                        | Hit::FocusThread
                        | Hit::FocusRename
                        | Hit::FocusEdit
                        | Hit::FocusEmojiSearch
                );
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
    /// drag-selection, and resolves an in-progress channel-row swipe (commit
    /// on a long swipe, click on a short one).
    fn handle_left_release(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut ui::widget::Edits<'_>,
        handled: &mut bool,
    ) {
        self.call.placement.end_drag(self.desktop_floating_bounds());
        self.end_agent_conversation_drag();
        // End any composer drag-selection in progress.
        self.forward_pointer_to_text_inputs(event, scale, modifiers, doc);
        if let Some(sw) = self.swipe.take() {
            let channel = self
                .ws_channels()
                .get(sw.channel_index)
                .map(|c| c.id.clone());
            if let Some(id) = channel {
                if sw.dx() >= self.chrome.swipe_commit {
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
        if self.rename.is_some() {
            if matches!(logical_key, Key::Named(NamedKey::Escape)) {
                self.cancel_rename();
            } else {
                self.route_rename_input(event, scale, modifiers, doc);
            }
            *handled = true;
            return;
        }
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
            && self.rename.is_none()
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

        if self.emoji_open && !matches!(logical_key, Key::Named(NamedKey::Escape)) {
            if matches!(logical_key, Key::Named(NamedKey::Enter)) {
                self.pick_first_emoji_result(doc);
            } else {
                self.route_emoji_search(event, scale, modifiers, doc);
            }
            *handled = true;
            return;
        }

        let composer_owns = self.main_composer_can_edit()
            && self.menu.is_none()
            && !self.emoji_open
            && self.history_open.is_none()
            && !self.attach_open
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
    /// commits still splice into the collaborative draft; inline edit/rename
    /// buffers remain local until their explicit Save/Enter boundary.
    fn handle_text_input_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut ui::widget::Edits<'_>,
    ) -> bool {
        if self.rename.is_some() {
            self.route_rename_input(event, scale, modifiers, doc);
            true
        } else if self.editing.is_some() {
            self.route_edit_input(event, scale, modifiers, doc);
            true
        } else if self.emoji_open {
            self.route_emoji_search(event, scale, modifiers, doc);
            true
        } else if self.main_composer_can_edit()
            && self.menu.is_none()
            && !self.emoji_open
            && self.history_open.is_none()
            && !self.attach_open
        {
            self.route_composer_key(event, scale, modifiers, doc);
            true
        } else {
            false
        }
    }

    fn route_rename_input(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        drive_text_input(&mut self.rename_input, event, scale, modifiers, doc);
        if self.rename_input.take_submit() {
            self.commit_rename(doc);
        } else {
            let _ = self.rename_input.take_dirty();
        }
    }

    fn route_emoji_search(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        drive_text_input(&mut self.emoji_search, event, scale, modifiers, doc);
        if self.emoji_search.take_dirty().is_some() {
            self.emoji_scroll = 0.0;
        }
    }

    /// Enter in the picker's search field picks the best match.
    fn pick_first_emoji_result(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let first = crate::i18n::emoji_catalogue()
            .search(self.emoji_search.value(), 1)
            .first()
            .map(|e| e.text());
        if let Some(first) = first {
            self.on_hit_emoji_choice(first, doc);
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
        let images = self.focused_composer_mut().take_pasted_images();
        self.queue_pasted_images(images);
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

    fn queue_pasted_images(&mut self, images: Vec<ui::clipboard::ClipboardImage>) {
        // The device queue posts to a room channel, never a private conversation.
        // Reject here rather than letting a later navigation publish private bytes.
        if self.direct_person.is_some() || self.direct_agent.is_some() || self.task_goal.is_some() {
            if !images.is_empty() {
                self.attachment_status = Some(crate::i18n::t("chat-private-images-unavailable"));
            }
            return;
        }
        for image in images {
            let extension = image.mime.rsplit('/').next().unwrap_or("png");
            self.device_files
                .push_back((format!("Pasted image.{extension}"), image.bytes));
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
        drive_text_input(&mut self.rename_input, event, scale, modifiers, doc);
        self.edit_input.handle(event, scale, modifiers, doc);
        if self.emoji_open {
            drive_text_input(&mut self.emoji_search, event, scale, modifiers, doc);
        }
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
        if self.emoji_open {
            let handled = drive_text_input(&mut self.emoji_search, event, scale, modifiers, doc);
            if self.emoji_search.take_dirty().is_some() {
                self.emoji_scroll = 0.0;
            }
            // A press elsewhere on the panel leaves typing in the query.
            self.emoji_search.set_focused(true);
            handled
        } else if self.rename.is_some() {
            drive_text_input(&mut self.rename_input, event, scale, modifiers, doc)
        } else if self.editing.is_some() {
            self.edit_input.handle(event, scale, modifiers, doc)
        } else {
            self.focused_composer_mut()
                .handle(event, scale, modifiers, doc)
        }
    }

    /// Right-click opens a context menu for the channel/category/workspace
    /// under the cursor.
    fn on_right_click(&mut self, hit: Hit) {
        let menu = match hit {
            Hit::Channel(id) | Hit::VoiceChannel(id) => Some(self.context_menu(
                MenuTarget::Channel(id),
                self.last_mouse,
                MenuAction::for_channel().to_vec(),
            )),
            Hit::CategoryToggle(id) | Hit::AddChannel { category: id, .. } => {
                Some(self.context_menu(
                    MenuTarget::Category(id),
                    self.last_mouse,
                    MenuAction::for_category().to_vec(),
                ))
            }
            // One of your own messages — offer the message actions (Edit).
            Hit::MessageMenu { msg } => {
                let mine = self.message(&msg).is_some_and(|m| self.is_me(&m.author));
                let author = self.message(&msg).map(|m| m.author.clone());
                let items = if mine {
                    MenuAction::for_message().to_vec()
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
            _ => Some(self.context_menu(
                MenuTarget::Workspace(self.active_ws.clone()),
                self.last_mouse,
                MenuAction::for_workspace().to_vec(),
            )),
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
            Hit::Inbox => {
                if self.direct_person.is_some()
                    || self.direct_agent.is_some()
                    || self.task_goal.is_some()
                {
                    self.leave_floated_agent_route();
                    self.close_person_conversation();
                }
                self.inbox_open = true;
                self.thread_open = None;
                self.focus = Focus::Main;
            }
            Hit::CurrentContext => {
                self.inbox_open = false;
                self.on_hit_workspace(self.context_ws.clone());
            }
            Hit::Channel(id) => self.on_hit_channel(id, doc),
            Hit::DirectPerson(public_key) => {
                if let Err(error) = self.open_person_conversation(&public_key) {
                    self.person_error = Some(error);
                }
            }
            Hit::NewTask => self.begin_task_goal(None),
            Hit::Tasks => {
                if self.task_goal.is_none() && self.direct_agent.is_none() {
                    self.begin_task_goal(None);
                }
            }
            Hit::AgentWindow => {
                if self.conversation_surface {
                    self.conversation_return_requested = true;
                } else {
                    self.float_agent_conversation();
                }
            }
            Hit::AgentIntent(crate::agent_tasks::AgentComposerIntent::Queue) => {
                self.begin_task_goal(None)
            }
            Hit::AgentIntent(intent) => self.set_direct_agent_intent(intent),
            Hit::AgentTask(frontier) => {
                self.select_agent_task(&frontier);
            }
            Hit::TaskLabel(label) => {
                self.task_label = label;
                self.request_relayout();
            }
            Hit::TaskFilter(filter) => {
                self.task_filter = filter;
                self.request_relayout();
            }
            Hit::TaskExpand(task) => self.toggle_task_children(&task),
            Hit::AgentSetup => {
                if self.agent_tasks.needs_setup() {
                    let _ = self.answer_effect(crate::effects::ChatEffect::SetupTaskModels);
                } else {
                    self.agent_tasks.refresh_compute(&self.active_ws);
                    self.request_relayout();
                }
            }
            Hit::AgentSubtask => {
                if let Some(parent) = self
                    .direct_agent_tasks()
                    .and_then(crate::agent_tasks::AgentTaskView::current)
                    .map(|task| task.task_id.clone())
                {
                    self.begin_task_goal(Some(parent));
                }
            }
            Hit::TaskOutput(output) => {
                self.agent_tasks.open_output(&output);
            }
            Hit::AgentTaskMore => {
                if let Some(agent) = self.direct_agent.clone() {
                    self.agent_tasks.next_page(&agent);
                }
            }
            Hit::AgentStart => {
                self.start_agent_task();
            }
            Hit::AgentStop => {
                self.stop_agent_task();
            }
            Hit::VoiceChannel(id) => self.join_voice(&id, doc),
            Hit::CategoryToggle(id) => self.toggle_category(&id),
            Hit::EditSave => self.commit_edit(doc),
            Hit::EditCancel => self.cancel_edit(),
            Hit::AddChannel {
                workspace,
                category,
            } if self.can_participate() => self.on_hit_add_channel(&workspace, &category),
            Hit::AddCategory(ws) if self.can_participate() => self.on_hit_add_to_workspace(&ws),
            Hit::FocusRename | Hit::FocusEdit => self.set_composer_focus(),
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
            Hit::EmojiGroup(group) => {
                self.emoji_group = group;
                self.emoji_scroll = 0.0;
                self.emoji_search.set_query("");
            }
            Hit::FocusEmojiSearch => self.emoji_search.set_focused(true),
            Hit::Attach if self.can_participate() => self.open_attach_picker(),
            Hit::AttachPick(path) => {
                self.attach_file(doc, std::path::Path::new(&path));
            }
            Hit::AttachDevice => self.choose_device_files(),
            Hit::StartCall if self.call_allows(crate::call::CallAction::Join) => {
                self.on_hit_start_call(doc);
            }
            Hit::StopSharingLiveContext => {
                // The same device-local handler the public effect reaches.
                let _ = self.answer_effect(crate::effects::ChatEffect::StopSharingLiveContext);
            }
            Hit::CopyResourceLink(message) => self.begin_resource_navigation(&message, false),
            Hit::OpenResourceLink(message) => self.begin_resource_navigation(&message, true),
            Hit::CloseThread => {
                self.close_thread();
            }
            Hit::CallParticipantsToggle => {
                self.call.participants_expanded = !self.call.participants_expanded;
            }
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
            Hit::CallExpand => {
                self.call.expanded = !self.call.expanded;
                self.request_relayout();
            }
            Hit::CallLeave => self.leave_voice(doc),
            Hit::Search => self
                .search_opener
                .open(ui::search::SearchRequest::this_app()),
            Hit::ToggleLeftFlap => self.left_flap.toggle(),
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
            Hit::Dismiss => self.on_hit_dismiss(),
            // MessageMenu is a right-click affordance and drag handles are
            // handled at press time; permission-gated fallthroughs are no-ops.
            _ => {}
        }
        self.reload(doc);
    }

    fn on_hit_workspace(&mut self, id: Id) {
        if self.direct_person.is_some() || self.direct_agent.is_some() || self.task_goal.is_some() {
            self.leave_floated_agent_route();
        }
        self.task_goal = None;
        self.close_person_conversation();
        if self.active_ws != id {
            self.active_ws = id;
            self.thread_open = None;
            self.reset_timeline_to_latest();
            self.focus = Focus::Main;
            self.active_channel.clear(); // force reload to pick first text channel
        }
    }

    fn on_hit_channel(&mut self, id: Id, doc: &mut ui::widget::Edits<'_>) {
        if self.direct_person.is_some() || self.direct_agent.is_some() || self.task_goal.is_some() {
            self.leave_floated_agent_route();
        }
        self.task_goal = None;
        self.close_person_conversation();
        self.not_sent = None;
        let kind = self.channel(&id).map(|c| c.kind);
        if kind == Some(ChannelKind::Voice) {
            self.join_voice(&id, doc);
        } else {
            if let Some(workspace) = self.channel(&id).map(|channel| channel.workspace.clone()) {
                self.active_ws = workspace;
            }
            self.active_channel = id;
            self.thread_open = None;
            self.reset_timeline_to_latest();
            self.focus = Focus::Main;
            // Picking a channel in overlay mode closes the left flap.
            if self.left_overlay() {
                self.left_flap.close();
            }
            // Navigation is the user-visible read boundary. Other controls in
            // an already-open room must not append a second MarkRead action
            // after their own canonical action or obscure it in history.
            self.mark_active_read(doc);
        }
    }

    fn on_hit_add_channel(&mut self, workspace: &str, category: &str) {
        self.open_channel_sheet(crate::channel_sheet::ChannelSheet::new_channel(
            workspace,
            category,
            ChannelKind::Text,
        ));
    }

    /// The sidebar header's "+": what can be made in the workspace.
    fn on_hit_add_to_workspace(&mut self, ws: &str) {
        self.menu = Some(self.context_menu(
            MenuTarget::Workspace(ws.to_owned()),
            self.last_mouse,
            MenuAction::for_workspace().to_vec(),
        ));
    }

    /// Ask what to make or change in `sheet`, over everything else.
    pub(crate) fn open_channel_sheet(&mut self, sheet: crate::channel_sheet::ChannelSheet) {
        self.menu = None;
        self.rename = None;
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
                        workspace: item(&answer.workspace),
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
                        workspace: item(&answer.workspace),
                        name: answer.name.clone(),
                    }),
                );
                self.reload(doc);
            }
            SheetMode::EditChannel(id) => {
                let Some(channel) = self.channel(id).cloned() else {
                    return;
                };
                let path = Chat::paths().channels.item(&item(id));
                if channel.name != answer.name {
                    self.apply_now(doc, path.name.set(answer.name.clone()));
                }
                if channel.topic != answer.topic {
                    self.apply_now(doc, path.topic.set(answer.topic.clone()));
                }
                let category = (!channel.category.is_empty()).then(|| channel.category.clone());
                if category != answer.category {
                    self.apply_now(doc, path.category.set(answer.category.as_deref().map(item)));
                }
                self.reload(doc);
            }
        }
    }

    /// Add a channel named `name` to `category` of `workspace` now, as an
    /// insert under an id minted here, so its inline rename can follow at
    /// once; a text channel comes with its draft.
    pub(crate) fn create_channel_now(
        &mut self,
        doc: &mut ui::widget::Edits<'_>,
        workspace: &str,
        category: &str,
        name: &str,
        kind: ChannelKind,
    ) -> String {
        let id = fresh_id("c");
        let channel = crate::record::Channel {
            workspace: item(workspace),
            category: (!category.is_empty()).then(|| item(category)),
            name: name.to_owned(),
            kind,
            ..crate::record::Channel::default()
        };
        let insert = Chat::paths()
            .channels
            .insert_as(&item(&id), self.channels.len(), &channel);
        self.apply_now(doc, insert);
        self.reload(doc);
        id
    }

    fn on_hit_reaction(&mut self, msg: &str, emoji: &str, doc: &mut ui::widget::Edits<'_>) {
        if self.can_participate() {
            let action = self.reaction_toggle_action(msg, emoji);
            self.apply_part(doc, action);
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
    fn open_emoji_picker(&mut self, target: Option<Id>, anchor: Point) {
        self.emoji_open = true;
        self.emoji_target = target;
        self.emoji_anchor = anchor;
        self.emoji_search.set_query("");
        let _ = self.emoji_search.take_dirty();
        self.emoji_search.set_focused(true);
        self.emoji_scroll = 0.0;
        // Space and Enter now type into the query rather than re-activating
        // the control that opened the picker.
        self.keyboard_target = None;
    }

    fn on_hit_emoji_choice(&mut self, e: &str, doc: &mut ui::widget::Edits<'_>) {
        self.emoji_recents.record(e);
        if let Some(target) = self.emoji_target.take() {
            if self.can_participate() {
                let action = self.reaction_toggle_action(&target, e);
                self.apply_part(doc, action);
                self.reload(doc);
            }
        } else {
            self.insert_text(doc, e);
        }
        self.emoji_open = false;
    }

    /// Join this workspace's voice channel; if it has none, make one to
    /// start the call in so the header "Call" button is never a dead end (a
    /// text-only workspace can still huddle).
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
        if self.attach_open {
            self.attach_open = false;
        } else if self.menu.is_some() {
            self.menu = None;
        } else if self.emoji_open {
            self.emoji_open = false;
            self.emoji_target = None;
        } else if self.history_open.is_some() {
            self.history_open = None;
        } else if self.left_overlay() && self.left_flap_open() {
            self.left_flap.close();
        } else if self.right_overlay() && self.right_flap_open() {
            self.right_flap.close();
        }
    }

    // --- voice / call wiring ---------------------------------------------

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

    // --- structure CRUD ---------------------------------------------------

    pub(crate) fn apply_menu_action(
        &mut self,
        target: MenuTarget,
        action: MenuAction,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        match target {
            MenuTarget::Channel(id) => self.apply_channel_action(&id, action, doc),
            MenuTarget::Category(id) => self.apply_category_action(&id, action, doc),
            MenuTarget::Message(id) => match action {
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
                MenuAction::DeleteMessage => {
                    let mine = self.message(&id).is_some_and(|m| self.is_me(&m.author));
                    if mine {
                        self.apply_part(
                            doc,
                            ChatPart::DELETE_MESSAGE
                                .with(chat_part::DeleteMessage { message: item(&id) }),
                        );
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
            MenuTarget::Workspace(ws) => match action {
                MenuAction::NewCategory => {
                    self.open_channel_sheet(crate::channel_sheet::ChannelSheet::new_category(&ws));
                }
                MenuAction::NewChannelHere => {
                    self.open_channel_sheet(crate::channel_sheet::ChannelSheet::new_channel(
                        &ws,
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
                let archive = Chat::paths()
                    .channels
                    .item(&item(id))
                    .archived
                    .set(!archived);
                self.apply_now(doc, archive);
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
        let ws = self.active_ws.clone();
        match action {
            MenuAction::Rename => self.begin_rename(MenuTarget::Category(id.to_string())),
            MenuAction::Delete => self.apply_now(
                doc,
                Chat::DELETE_CATEGORY.with(chat::DeleteCategory { category: item(id) }),
            ),
            MenuAction::ToggleCollapse => self.toggle_category(id),
            MenuAction::NewChannelHere => self.open_channel_sheet(
                crate::channel_sheet::ChannelSheet::new_channel(&ws, id, ChannelKind::Text),
            ),
            MenuAction::NewVoiceChannelHere => self.open_channel_sheet(
                crate::channel_sheet::ChannelSheet::new_channel(&ws, id, ChannelKind::Voice),
            ),
            _ => {}
        }
        self.reload(doc);
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
        self.rename_input.set_text(&current);
        self.rename_input.set_cursor_pos(current.len());
        self.rename = Some(target);
        self.set_composer_focus();
    }

    /// Commit the inline rename to the CRDT and close the editor.
    fn commit_rename(&mut self, doc: &mut ui::widget::Edits<'_>) {
        if let Some(target) = self.rename.take() {
            let text = self.rename_input.value().trim().to_string();
            if !text.is_empty() {
                match target {
                    MenuTarget::Channel(id) => {
                        let name = text.trim_start_matches('#').to_owned();
                        let rename = Chat::paths().channels.item(&item(&id)).name.set(name);
                        self.apply_now(doc, rename);
                    }
                    MenuTarget::Category(id) => {
                        let rename = Chat::paths().categories.item(&item(&id)).name.set(text);
                        self.apply_now(doc, rename);
                    }
                    MenuTarget::Message(_) | MenuTarget::Workspace(_) => {}
                }
            }
            self.rename_input.set_text("");
            self.set_composer_focus();
            self.reload(doc);
        }
    }

    fn cancel_rename(&mut self) {
        self.rename = None;
        self.rename_input.set_text("");
        self.set_composer_focus();
    }

    fn on_key(&mut self, key: &Key, modifiers: Modifiers, doc: &mut ui::widget::Edits<'_>) -> bool {
        // Escape unwinds the top-most layer.
        if matches!(key, Key::Named(NamedKey::Escape)) {
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
                self.close_thread();
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
        if self.task_goal.is_some() {
            self.save_task_goal();
            return;
        }
        if self.direct_person.is_some() {
            self.send_person_focused();
            return;
        }
        if self.direct_agent.is_some() {
            self.send_agent_focused(doc);
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
                channel: item(&self.active_channel),
                text,
                parent: parent.as_deref().map(item),
                body: Some(body),
                attachment: None,
                mentions: None,
                written: None,
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
            let mut own = automerge::AutoCommit::new();
            self.composer_main.ensure_draft(&own);
            self.composer_main
                .insert_text(&mut ui::widget::Edits::lend(&mut own), &text);
        }
    }

    pub(crate) fn send_agent_focused(&mut self, doc: &ui::widget::Edits<'_>) -> bool {
        let Some(agent) = self.direct_agent.clone() else {
            return false;
        };
        if !self.direct_agent_intent_allowed(self.direct_agent_intent()) {
            return false;
        }
        let text = self.composer_main.private_plain().unwrap_or_default();
        if text.trim().is_empty() {
            return false;
        }
        let Some(route) = self.agent_draft_route() else {
            return false;
        };
        let Some(revision) = self
            .composer_main
            .private_revision_for(&self.active_ws, &route)
        else {
            return false;
        };
        let accepted = match self.direct_agent_intent() {
            crate::agent_tasks::AgentComposerIntent::Steer => {
                self.agent_tasks.direct(&agent, text, revision)
            }
            crate::agent_tasks::AgentComposerIntent::Queue => {
                self.agent_tasks.queue(&agent, text, route, revision)
            }
        };
        if accepted {
            self.jump_to_latest();
        }
        self.reload(doc);
        accepted
    }

    pub(crate) fn select_agent_task(
        &mut self,
        frontier: &crate::agent_tasks::AgentTaskFrontier,
    ) -> bool {
        let agent = frontier.assigned_agent.clone();
        if self.direct_agent.as_deref() != Some(agent.as_str())
            && !self.open_agent_conversation(&agent)
        {
            return false;
        }
        let selected = self.agent_tasks.select(&agent, frontier);
        if selected {
            self.agent_intents.update(|intents| {
                intents.insert(
                    (self.active_ws.clone(), agent),
                    crate::agent_tasks::AgentComposerIntent::Steer,
                );
            });
            self.sync_task_channel();
            self.sync_agent_route();
            let channel = self.active_channel.clone();
            self.restore_attached_agent_surface(&channel);
            self.request_relayout();
        }
        selected
    }

    pub(crate) fn start_agent_task(&mut self) -> bool {
        let Some(agent) = self.direct_agent.clone() else {
            return false;
        };
        self.agent_tasks.start(&agent)
    }

    pub(crate) fn stop_agent_task(&mut self) -> bool {
        let Some(agent) = self.direct_agent.clone() else {
            return false;
        };
        self.agent_tasks.stop(&agent)
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
        // Without a workspace on this device (a browser page) the files to
        // share are the person's own: the platform's chooser opens at once.
        if !self.external_services {
            self.choose_device_files();
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
            ui::portal::PickRequest::open_multiple("Share files", "Share")
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
            channel: item(&pending.channel),
            text: String::new(),
            parent: None,
            body: None,
            attachment: Some(crate::record::AttachmentUpdate::of(&attachment)),
            mentions: None,
            written: None,
        }));
        self.scroll = 0.0;
        self.attachment_status = Some("Attachment shared".into());
    }
}

fn prepare_attachment_import(
    content: &tilo_content::ContentStore,
    token: u64,
    path: &std::path::Path,
) -> AttachmentImportResult {
    let fail = |reason: &str| AttachmentImportResult::Failed {
        token,
        reason: reason.into(),
    };
    let Ok((dir, name)) = tilo_files::file_at(path) else {
        return fail("the file is no longer there; choose it again");
    };
    let limit = usize::try_from(crate::record::MAX_ATTACHMENT_BYTES).unwrap_or(usize::MAX);
    let bytes = match tilo_files::blocking::read_bounded(&dir, &name, limit) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return fail("files up to 32 MiB can be shared"),
        Err(_) => return fail("the file could not be read; check it still exists"),
    };
    let name = path
        .file_name()
        .map_or_else(|| "file".into(), |name| name.to_string_lossy().into_owned());
    store_attachment(content, token, name, &bytes)
}

/// Keep `bytes`, a file named `name`, as shared content: what an attachment
/// refers to, whichever way the file arrived.
fn store_attachment(
    content: &tilo_content::ContentStore,
    token: u64,
    name: String,
    bytes: &[u8],
) -> AttachmentImportResult {
    let fail = |reason: &str| AttachmentImportResult::Failed {
        token,
        reason: reason.into(),
    };
    if bytes.is_empty() {
        return fail("the file is empty");
    }
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > crate::record::MAX_ATTACHMENT_BYTES {
        return fail("files up to 32 MiB can be shared");
    }
    let meta = tilo_content::sniff(bytes);
    let blob = match content.put(bytes) {
        Ok(blob) => blob,
        Err(error) => {
            eprintln!("chat: content store refused an attachment: {error}");
            return fail("there is no room to keep it here; free some space and try again");
        }
    };
    if content.read(&blob).as_deref() != Some(bytes) {
        return fail("the stored copy did not match; try again");
    }
    let video = crate::picker::is_video_name(&name);
    AttachmentImportResult::Ready {
        token,
        name,
        blob,
        size: u64::try_from(bytes.len()).unwrap_or(crate::record::MAX_ATTACHMENT_BYTES),
        width: meta.w,
        height: meta.h,
        video,
    }
}

/// Run a bounded job on its own thread, or inline where no thread can start
/// (a browser page, where the job's workspace reads fail fast). A native
/// thread that cannot start leaves the reason on the status line.
fn run_worker(name: &str, job: impl FnOnce() + Send + 'static, status: &mut Option<String>) {
    if !ui::rt::available(ui::rt::Feature::Threads) {
        job();
        return;
    }
    if let Err(error) = ui::rt::spawn_thread(name, job) {
        *status = Some(format!("Attachments unavailable: {error}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model;
    use tilo_automerge::ApplyIncoming;

    fn service_streams(app: &mut ChatApp, channel: &str, count: usize) {
        app.native_voice
            .submit(ui::native_media::Command::Join {
                channel_id: channel.into(),
            })
            .unwrap();
        let requests = app.native_voice.take_requests(ui::time::now());
        let id = requests
            .iter()
            .rev()
            .find_map(|request| match request {
                ui::collaboration::CollabRequest::NativeMediaCommand {
                    request_id,
                    command,
                } if matches!(command.command, ui::native_media::Command::Join { .. }) => {
                    Some(*request_id)
                }
                _ => None,
            })
            .unwrap();
        let mut snapshot = ui::native_media::Snapshot::unavailable();
        snapshot.state = ui::native_media::State::Live;
        snapshot.issue = None;
        snapshot.inventory_revision = Some(1);
        snapshot.publishers = (1..=count)
            .map(|index| format!("{}/{index:064x}", "11".repeat(32)))
            .collect();
        assert!(snapshot.valid());
        app.native_voice.complete(id, &Ok(snapshot));
        app.project_native_voice();
    }

    fn app_with_doc() -> (ChatApp, crate::testing::Room) {
        let doc = crate::testing::demo_doc();
        let mut app = ChatApp::new_ephemeral();
        app.attach_room(&doc);
        app.reload(&doc);
        (app, doc)
    }

    /// Exercise a domain action through the retained menu's semantic
    /// activation channel—the same selection channel used by Enter/click.
    fn activate_context_menu_action(
        app: &mut ChatApp,
        doc: &mut AutoCommit,
        target: MenuTarget,
        action: MenuAction,
    ) {
        let items = match &target {
            MenuTarget::Channel(_) => MenuAction::for_channel().to_vec(),
            MenuTarget::Category(_) => MenuAction::for_category().to_vec(),
            MenuTarget::Message(_) => MenuAction::for_message().to_vec(),
            MenuTarget::Workspace(_) => MenuAction::for_workspace().to_vec(),
        };
        let index = items
            .iter()
            .position(|candidate| *candidate == action)
            .expect("action belongs to this menu");
        app.rect = vello::kurbo::Rect::new(0.0, 0.0, 900.0, 700.0);
        app.menu = Some(app.context_menu(target, Point::new(120.0, 120.0), items));
        assert!(ui::Widget::perform_semantic_action(
            app,
            &format!("context-menu:item:{index}"),
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(doc),
            &automerge::ROOT,
        ));
        assert!(app.menu.is_none(), "selection closes the modal menu");
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one labelled phase per original scenario, each with its own fresh fixture"
    )]
    fn channel_context_menu_and_swipe_delete_through_the_palette_action() {
        // Phase: delete_channel_affordance_matches_palette_spec.
        {
            let (mut app, mut doc) = app_with_doc();

            // The palette's delete spec for c_crit (built before the mutation).
            let state = <crate::resource::Chat as ui::tilo_resource::Resource>::read(&doc).unwrap();
            let crit = <crate::resource::Chat as ui::tilo_resource::Resource>::child(
                &state,
                &crate::testing::item("channels", "c_crit"),
            )
            .expect("c_crit is a channel resource");
            let offered = tilo_actions::door::survey(std::slice::from_ref(&crit))
                .into_iter()
                .find(|offered| offered.address.action == "delete-channel")
                .expect("the channel offers delete-channel");
            let from_spec = tilo_actions::door::encode_json(
                std::slice::from_ref(&crit),
                &offered.address,
                &serde_json::Map::new(),
            )
            .expect("the channel offers delete-channel with itself bound");
            let from_spec: serde_json::Value = serde_json::from_slice(&from_spec.payload).unwrap();
            assert_eq!(
                from_spec["op"],
                crate::testing::operation(
                    &crate::resource::Chat::DELETE_CHANNEL
                        .with(ui::tilo_resource::Id::from_raw("c_crit"))
                ),
                "the palette's delete-channel is the record's verb on that channel"
            );

            // Drive the real on-screen affordance (right-click menu → Delete).
            activate_context_menu_action(
                &mut app,
                &mut doc,
                MenuTarget::Channel("c_crit".into()),
                MenuAction::Delete,
            );
            let dispatched = app
                .last_dispatched()
                .expect("the affordance dispatched an action");

            assert_eq!(
                serde_json::to_value(&dispatched).unwrap(),
                from_spec["op"],
                "the delete-channel affordance and its palette action build the identical action"
            );
            // And it really deleted the channel once the person confirmed (the
            // affordance is on the real path).
            app.confirm_asked(&mut doc);
            assert!(
                crate::testing::channels(&doc)
                    .into_iter()
                    .all(|c| c.id != "c_crit")
            );
        }

        // Phase: context_menu_delete_removes_a_channel.
        {
            let (mut app, mut doc) = app_with_doc();
            assert!(app.channels.iter().any(|c| c.id == "c_random"));
            activate_context_menu_action(
                &mut app,
                &mut doc,
                MenuTarget::Channel("c_random".into()),
                MenuAction::Delete,
            );
            app.confirm_asked(&mut doc);
            assert!(
                !app.channels.iter().any(|c| c.id == "c_random"),
                "channel deleted via menu"
            );
        }

        // Phase: context_menu_is_modal_keyboard_operable_and_accessible.
        {
            let (mut app, mut doc) = app_with_doc();
            app.rect = vello::kurbo::Rect::new(0.0, 0.0, 900.0, 700.0);
            app.menu = Some(app.context_menu(
                MenuTarget::Channel("c_random".into()),
                Point::new(850.0, 680.0),
                MenuAction::for_channel().to_vec(),
            ));

            let root = ui::Widget::accessibility(&app).expect("Chat semantic root");
            let menu = root
                .children
                .iter()
                .find(|node| node.role == ui::a11y::Role::Menu)
                .expect("open shared menu is in Chat's semantic tree");
            assert_eq!(menu.children.len(), MenuAction::for_channel().len());
            assert_eq!(menu.children[0].name.as_deref(), Some("Edit channel"));
            assert_eq!(menu.children[1].name.as_deref(), Some("Rename channel"));
            assert_eq!(menu.children[2].name.as_deref(), Some("Archive channel"));
            assert_eq!(
                menu.children[5].name.as_deref(),
                Some("Delete channel"),
                "the destructive action comes last"
            );
            assert!(ui::Widget::traps_focus(&app));
            let capture = ui::Widget::modal_capture(&app).expect("menu captures input");
            assert!(app.rect.contains(capture.center()));

            // The first row is the safe action the keyboard lands on.
            for key in [NamedKey::ArrowDown, NamedKey::Enter] {
                let mut handled = false;
                app.dispatch_event(
                    &named_key(key),
                    1.0,
                    Modifiers::default(),
                    &mut ui::widget::Edits::lend(&mut doc),
                    &mut handled,
                );
                assert!(handled);
            }
            assert!(app.menu.is_none());
            assert!(
                app.channel_sheet.is_some(),
                "Enter activates the safely focused non-destructive action"
            );
        }

        // Phase: swipe_left_action_matches_context_menu_primary.
        {
            // The parity invariant: a left-swipe resolves to the channel
            // context menu's destructive action.
            assert!(MenuAction::for_channel().contains(&MenuAction::channel_swipe_left()));
            assert_eq!(MenuAction::channel_swipe_left(), MenuAction::Delete);
        }

        // Phase: swipe_left_on_a_channel_row_deletes_it.
        {
            let (mut app, mut doc) = app_with_doc();
            assert!(app.channels.iter().any(|c| c.id == "c_random"));
            // Draw once so the channel rows are hit-tested (as the live loop does).
            render_frame(&mut app, 1280.0, 720.0);
            let row = app
                .hits
                .iter()
                .find(|(_, h)| matches!(h, Hit::Channel(id) if id == "c_random"))
                .map(|(r, _)| *r)
                .expect("the #random row was hit-tested");
            let cy = row.center().y;
            // Press on the row (arms the swipe), drag left past the commit
            // threshold, release (commits the primary action = Delete).
            feed(&mut app, &mut doc, &cursor_moved(row.center().x, cy));
            feed(
                &mut app,
                &mut doc,
                &mouse(ElementState::Pressed, MouseButton::Left),
            );
            assert!(app.swipe.is_some(), "pressing a channel row arms a swipe");
            let past_commit = row.center().x - (app.chrome.swipe_commit + 20.0);
            feed(&mut app, &mut doc, &cursor_moved(past_commit, cy));
            feed(
                &mut app,
                &mut doc,
                &mouse(ElementState::Released, MouseButton::Left),
            );
            app.confirm_asked(&mut doc);
            assert!(
                !app.channels.iter().any(|c| c.id == "c_random"),
                "the committed swipe deleted the channel"
            );
        }
    }

    #[test]
    fn with_no_channel_open_the_composer_takes_no_text() {
        // A chat with no channels yet, as a room opens one its operator named
        // none for.
        let mut doc = AutoCommit::new();
        let mut app = ChatApp::new_ephemeral();
        app.reload(&doc);
        assert!(
            app.channel(&app.active_channel).is_none(),
            "no channel is open"
        );
        app.focus = Focus::Main;
        app.set_composer_focus();
        type_str(&mut app, &mut doc, "nowhere to go");
        assert_eq!(app.composer_main.value(), "", "no channel, no draft");
        feed(&mut app, &mut doc, &named_key(NamedKey::Enter));
        assert!(
            !app.dispatched_parts
                .borrow()
                .iter()
                .map(|action| serde_json::to_value(action).unwrap())
                .any(|action| action["action"] == "post"),
            "nothing was posted"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one labelled phase per original scenario, each with its own fresh fixture"
    )]
    fn composer_edits_the_draft_and_sends_through_the_action_route() {
        // Phase: send_affordance_dispatches_a_send_message_action.
        {
            let (mut app, mut doc) = app_with_doc();
            app.active_channel = "c_general".into();
            app.focus = Focus::Main;
            app.thread_open = None;
            crate::testing::write_draft(&mut app, &mut doc, "hi from the composer");
            app.on_hit(Hit::Send, &mut ui::widget::Edits::lend(&mut doc));
            let action = app
                .dispatched_parts
                .borrow()
                .iter()
                .map(|action| serde_json::to_value(action).unwrap())
                .find(|action| action["action"] == "post")
                .expect("send dispatched a post");
            assert_eq!(action["args"]["channel"], "c_general");
            assert_eq!(action["args"]["text"], "hi from the composer");
        }

        // Phase: typing_then_send_appends_a_message.
        {
            let (mut app, mut doc) = app_with_doc();
            let before = crate::testing::messages(&doc, "c_crit").len();
            app.focus = Focus::Main;
            app.set_composer_focus();
            // Type through the real composer TextInput, then Enter to send
            // (multiline + enter-submits: plain Enter sends).
            type_str(&mut app, &mut doc, "hello team");
            assert_eq!(
                app.composer_main.value(),
                "hello team",
                "composer holds the typed text"
            );
            assert_eq!(
                app.draft_main, "hello team",
                "the draft is mirrored into the CRDT"
            );
            feed(&mut app, &mut doc, &named_key(NamedKey::Enter));
            let after = crate::testing::messages(&doc, "c_crit");
            assert_eq!(after.len(), before + 1);
            assert_eq!(after.last().unwrap().text, "hello team");
            assert_eq!(app.composer_main.value(), "", "composer cleared after send");
            assert_eq!(app.draft_main, "", "draft cleared after send");
        }

        // Phase: backspace_edits_the_draft.
        {
            let (mut app, mut doc) = app_with_doc();
            app.focus = Focus::Main;
            app.set_composer_focus();
            type_str(&mut app, &mut doc, "abc");
            feed(&mut app, &mut doc, &named_key(NamedKey::Backspace));
            assert_eq!(
                app.composer_main.value(),
                "ab",
                "backspace removed the last char"
            );
            assert_eq!(app.composer_main.caret(), 2, "caret follows the deletion");
            assert_eq!(app.draft_main, "ab", "the draft mirrors the edit");
        }

        // Phase: ctrl_v_pastes_clipboard_into_the_composer.
        {
            let (mut app, mut doc) = app_with_doc();
            app.focus = Focus::Main;
            app.set_composer_focus();

            let sentinel = "pasted from the clipboard";
            ui::clipboard_copy(sentinel);
            assert_eq!(
                ui::clipboard_paste().as_deref(),
                Some(sentinel),
                "clipboard round-trips here (precondition for the paste path)"
            );

            feed_mods(&mut app, &mut doc, &char_key("v"), mods(true, false));

            assert_eq!(
                app.composer_main.value(),
                sentinel,
                "Ctrl+V landed the clipboard text in the composer"
            );
            assert_eq!(
                app.draft_main, sentinel,
                "the pasted draft is mirrored into the CRDT"
            );
        }

        // Phase: drag_select_then_ctrl_c_copies_the_selection.
        {
            let (mut app, mut doc) = app_with_doc();
            app.focus = Focus::Main;
            app.set_composer_focus();
            type_str(&mut app, &mut doc, "copy me please");
            // A frame lays the composer out + populates its glyph metrics so a
            // pointer hit-test resolves a caret position.
            render_frame(&mut app, 1280.0, 720.0);

            // Select the whole value (Ctrl+A) then copy it (Ctrl+C).
            feed_mods(&mut app, &mut doc, &char_key("a"), mods(true, false));
            ui::clipboard_copy("stale"); // ensure the copy overwrites something
            feed_mods(&mut app, &mut doc, &char_key("c"), mods(true, false));
            assert_eq!(
                ui::clipboard_paste().as_deref(),
                Some("copy me please"),
                "Ctrl+C copied the selected composer text to the clipboard"
            );
        }

        // Phase: composer_emoji_pick_inserts_into_the_draft.
        {
            let (mut app, mut doc) = app_with_doc();
            app.focus = Focus::Main;
            assert!(app.draft_main.is_empty());
            app.on_hit(
                Hit::ComposerEmoji(Point::ZERO),
                &mut ui::widget::Edits::lend(&mut doc),
            );
            assert!(
                app.emoji_open && app.emoji_target.is_none(),
                "the composer picker has no reaction target"
            );
            app.on_hit(
                Hit::EmojiChoice("🎉".into()),
                &mut ui::widget::Edits::lend(&mut doc),
            );
            assert_eq!(
                app.draft_main, "🎉",
                "the emoji was typed into the composer draft"
            );
            assert_eq!(app.emoji_recents.items(), ["🎉"], "picks are remembered");
        }

        // Phase: reply_in_thread_sets_parent.
        {
            let (mut app, mut doc) = app_with_doc();
            app.thread_open = Some("m_concept".into());
            app.focus = Focus::Thread;
            app.insert_text(&mut ui::widget::Edits::lend(&mut doc), "nice");
            app.send_focused(&mut ui::widget::Edits::lend(&mut doc));
            let replies =
                model::reply_count(&crate::testing::messages(&doc, "c_crit"), "m_concept");
            assert_eq!(
                replies, 4,
                "thread gained one reply on top of the seeded three"
            );
        }
    }

    #[test]
    fn agent_private_drafts_keep_exact_identity_and_never_enter_the_room_document() {
        let (mut app, mut doc) = app_with_doc();
        app.room_agents = vec!["reed".into(), "scribe".into()];

        assert!(app.open_agent_conversation("scribe"));
        app.composer_main.sync(&doc);
        app.focus = Focus::Main;
        app.set_composer_focus();
        type_str(&mut app, &mut doc, "private for scribe");
        app.composer_main.select(8, 18);
        let scribe_selection = app.composer_main.selection();
        assert!(!crate::projection::has_draft(&doc, &app.active_channel));

        assert!(app.open_agent_conversation("reed"));
        app.composer_main.sync(&doc);
        assert_eq!(app.composer_main.value(), "");
        type_str(&mut app, &mut doc, "private for reed");
        assert!(!crate::projection::has_draft(&doc, &app.active_channel));

        assert!(app.open_agent_conversation("scribe"));
        app.composer_main.sync(&doc);
        assert_eq!(app.composer_main.value(), "private for scribe");
        assert_eq!(app.composer_main.selection(), scribe_selection);
        assert!(!crate::projection::has_draft(&doc, &app.active_channel));
    }

    #[test]
    fn a_refused_post_keeps_its_text_and_says_it_was_not_sent() {
        let (mut app, mut doc) = app_with_doc();
        app.active_channel = "c_general".into();
        app.focus = Focus::Main;
        app.thread_open = None;
        let before = crate::testing::top_level(&doc, "c_general");
        let too_long = "x".repeat(crate::record::MAX_MESSAGE_TEXT_BYTES + 1);
        crate::testing::write_draft(&mut app, &mut doc, &too_long);
        app.on_hit(Hit::Send, &mut ui::widget::Edits::lend(&mut doc));
        assert_eq!(crate::testing::top_level(&doc, "c_general"), before);
        assert_eq!(
            app.composer_main.value(),
            too_long,
            "the text waits to be sent again"
        );
        assert!(
            app.not_sent.is_some(),
            "the refusal is shown, never swallowed"
        );
        render_frame(&mut app, 1280.0, 720.0);
        let retries = app
            .hits
            .iter()
            .filter(|(rect, hit)| {
                matches!(hit, Hit::Send) && rect.y0 >= app.composer_main.rect().y1
            })
            .count();
        assert_eq!(retries, 1, "Retry stands under the composer");
    }

    /// A post the room refuses after this window showed it as sent comes
    /// back to the composer, marked as not sent.
    #[test]
    fn a_post_the_room_refuses_comes_back_to_the_composer() {
        let (mut app, mut doc) = app_with_doc();
        app.active_channel = "c_general".into();
        app.focus = Focus::Main;
        app.thread_open = None;
        crate::testing::write_draft(&mut app, &mut doc, "over the budget");
        app.on_hit(Hit::Send, &mut ui::widget::Edits::lend(&mut doc));
        assert_eq!(
            app.composer_main.value(),
            "",
            "sent, as far as this window knows"
        );
        let refused = app
            .chain
            .as_ref()
            .expect("the window holds the messages")
            .dispatcher()
            .refused();
        refused.set(Some(ui::framework::Refused {
            action: "post".into(),
            item: None,
            reason: "the room is receiving too many changes from you".into(),
            by_room: true,
        }));
        app.settle_refused_post();
        assert_eq!(app.composer_main.value(), "over the budget");
        assert_eq!(
            app.not_sent.as_deref(),
            Some("the room is receiving too many changes from you")
        );
    }

    #[test]
    fn semantic_send_reports_acceptance_only_when_it_appends_a_message() {
        let (mut app, mut doc) = app_with_doc();
        app.active_channel = "c_general".into();
        app.focus = Focus::Main;
        app.thread_open = None;
        render_frame(&mut app, 1280.0, 720.0);
        let target = app
            .semantic_hits()
            .into_iter()
            .find_map(|(_, hit, ..)| matches!(hit, Hit::Send).then(|| hit.semantic_id()))
            .expect("the painted Send button has a semantic target");

        assert!(!ui::Widget::perform_semantic_action(
            &mut app,
            &target,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        let before = crate::testing::top_level(&doc, "c_general");
        crate::testing::write_draft(&mut app, &mut doc, "accepted semantic send");
        render_frame(&mut app, 1280.0, 720.0);
        assert!(ui::Widget::perform_semantic_action(
            &mut app,
            &target,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!(crate::testing::top_level(&doc, "c_general"), before + 1);
        assert_eq!(app.composer_main.value(), "");
        assert!(
            !crate::projection::has_draft(&doc, "c_general"),
            "a draft never enters the room's record"
        );
    }

    #[test]
    fn live_start_workspace_switch_and_inbox_navigate_real_conversations() {
        // Phase: live_starter_selects_first_real_workspace_and_channel.
        {
            // Against a real (minimal) starter document whose ids differ from the
            // demo defaults the app boots with, `ensure_selection` must re-home
            // the active workspace/channel and drop the stale demo thread.
            let mut doc = AutoCommit::new();
            crate::testing::starter(&mut doc, model::ME, "You", "Y");
            let mut app = ChatApp::new_ephemeral(); // bare view state → demo ids
            app.reload(&doc);
            if app.ensure_selection() {
                app.reload(&doc);
            }
            assert_eq!(app.active_ws, "ws_home", "re-homed to the real workspace");
            assert_eq!(
                app.active_channel, "c_general",
                "picked the first text channel"
            );
            assert!(app.thread_open.is_none(), "stale demo thread dropped");
            // And nothing fake leaked in.
            assert!(app.users.iter().all(|u| u.id == model::ME));
        }

        // Phase: workspace_switch_resets_view_state.
        {
            let (mut app, doc) = app_with_doc();
            app.thread_open = Some("m_concept".into());
            app.scroll = -250.0;
            app.focus = Focus::Thread;
            app.on_hit_workspace("ws_tilo".into());
            app.reload(&doc);
            assert_eq!(app.active_ws, "ws_tilo");
            assert!(
                app.thread_open.is_none(),
                "switching workspaces drops the open thread"
            );
            assert_eq!(app.scroll, 0.0, "scroll resets");
            assert_eq!(app.focus, Focus::Main, "focus returns to the main composer");
            assert_eq!(
                app.channel(&app.active_channel).unwrap().workspace,
                "ws_tilo",
                "landed on a channel of the new workspace"
            );
        }

        // Phase: inbox_opens_cross_workspace_conversation_without_losing_context.
        {
            let (mut app, mut doc) = app_with_doc();
            let context = app.context_ws.clone();
            let cross_workspace = app
                .channels
                .iter()
                .find(|channel| channel.workspace != context && channel.kind == ChannelKind::Text)
                .expect("demo data has a cross-workspace conversation")
                .clone();

            app.on_hit(Hit::Inbox, &mut ui::widget::Edits::lend(&mut doc));
            app.on_hit(
                Hit::Channel(cross_workspace.id),
                &mut ui::widget::Edits::lend(&mut doc),
            );
            assert!(app.inbox_open);
            assert_eq!(app.active_ws, cross_workspace.workspace);
            assert_eq!(app.context_ws, context, "Inbox never rewrites OS context");

            app.on_hit(Hit::CurrentContext, &mut ui::widget::Edits::lend(&mut doc));
            assert!(!app.inbox_open);
            assert_eq!(app.active_ws, context);
        }

        // Phase: tab_and_enter_activate_the_visible_inbox_control.
        {
            let (mut app, mut doc) = app_with_doc();
            render_frame(&mut app, 1280.0, 720.0);

            let inbox_index = app
                .semantic_hits()
                .iter()
                .position(|(_, hit, ..)| matches!(hit, Hit::Inbox))
                .expect("Inbox is in the visible keyboard inventory");
            for _ in 0..=inbox_index {
                feed(&mut app, &mut doc, &named_key(NamedKey::Tab));
            }
            assert_eq!(app.keyboard_target, Some(inbox_index));
            feed(&mut app, &mut doc, &named_key(NamedKey::Enter));

            assert!(app.inbox_open, "Enter follows the first visible Tab target");
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one labelled phase per original scenario, each with its own fresh fixture"
    )]
    fn attachments_are_picked_imported_and_posted_as_real_portable_cards() {
        // Phase: attachment_image_decodes_through_the_image_store.
        {
            // The concept message in #design-crit carries an image attachment;
            // prewarming must drive it through the real fetch → decode → cache
            // path so a frame paints a decoded thumbnail (not a placeholder).
            let (app, _doc) = app_with_doc();
            assert!(
                app.messages.iter().any(|m| matches!(
                    &m.attachment,
                    Some(crate::model::Attachment::Image { blob, .. }) if blob == crate::images::DEMO_CALL_GRID
                )),
                "seeded conversation has an image attachment"
            );
            app.request_visible_images();
            app.images
                .block_until_idle(ui::time::Duration::from_secs(3));
            assert!(
                app.images.get(crate::images::DEMO_CALL_GRID).is_some(),
                "the shared ImageStore decoded the attachment thumbnail"
            );
        }

        // Phase: attach_shares_a_real_file_with_true_dimensions_and_no_caption.
        {
            // The picker imports the selected file into the content store, with
            // true header dimensions and no invented caption.
            let (mut app, mut doc) = app_with_doc();
            let dir = tempfile::tempdir().unwrap();
            let png = dir.path().join("diagram.png");
            write_real_png(&png, 240, 160);

            let before = crate::testing::messages(&doc, "c_crit").len();
            app.on_hit(
                Hit::AttachPick(png.to_string_lossy().into_owned()),
                &mut ui::widget::Edits::lend(&mut doc),
            );
            assert_eq!(
                crate::testing::messages(&doc, "c_crit").len(),
                before,
                "selecting only accepts a bounded local import; sync authors the message"
            );
            settle_attachment_until(&mut app, &mut doc, "c_crit", before + 1);
            let msgs = crate::testing::messages(&doc, "c_crit");
            assert_eq!(msgs.len(), before + 1, "one real attachment posted");
            let last = msgs.last().unwrap();
            assert_eq!(last.text, "", "no invented caption");
            match &last.attachment {
                Some(model::Attachment::Image { blob, w, h, .. }) => {
                    assert_eq!(blob.len(), 64, "posts a content hash");
                    assert!(app.content.has(blob));
                    assert_eq!(
                        (*w, *h),
                        (240, 160),
                        "reads the file's true header dimensions"
                    );
                }
                other => panic!("expected a real image attachment, got {other:?}"),
            }
        }

        // Phase: attach_button_opens_picker_listing_real_workspace_files.
        {
            let (mut app, mut doc) = app_with_doc();
            let dir = tempfile::tempdir().unwrap();
            write_real_png(&dir.path().join("shot.png"), 64, 64);
            // Point the workspace picker at the temp dir and open it via the real
            // event path (the composer "+").
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_CHAT_ATTACH_DIR", dir.path());
            app.on_hit(Hit::Attach, &mut ui::widget::Edits::lend(&mut doc));
            drop(env);
            assert!(app.attach_open, "the picker opens");
            assert!(
                app.attachment_picker_pending.is_some(),
                "directory work is off-thread"
            );
            settle_picker_until(&mut app);
            assert!(
                app.attach_entries.iter().any(|e| e.name == "shot.png"),
                "a real workspace image is listed, not a synthesized stand-in"
            );
        }

        // Phase: picker_result_settlement_is_generation_checked_and_cancelled_when_dismissed.
        {
            let (mut app, doc) = app_with_doc();
            app.attach_open = true;
            app.attachment_picker_pending = Some(7);
            app.attachment_picker_result_tx
                .try_send(AttachmentPickerResult {
                    generation: 6,
                    entries: Vec::new(),
                })
                .unwrap();
            app.settle_attachment_picker();
            assert_eq!(app.attachment_picker_pending, Some(7));

            // Reopening while a result is pending does not create a second scan.
            app.attachment_picker_generation = 12;
            app.open_attach_picker();
            assert_eq!(app.attachment_picker_generation, 12);
            assert_eq!(app.attachment_picker_pending, Some(7));

            // A dismissed picker consumes its matching worker answer as a
            // cancellation, rather than leaking the bounded worker slot.
            app.attach_open = false;
            app.attachment_picker_result_tx
                .try_send(AttachmentPickerResult {
                    generation: 7,
                    entries: Vec::new(),
                })
                .unwrap();
            app.settle_attachment_picker();
            assert!(app.attachment_picker_pending.is_none());
            assert_eq!(
                app.attachment_status.as_deref(),
                Some("Attachment picker cancelled")
            );

            // Keep the test on the same real event/sync surface as other picker
            // tests: a cancellation never authors a document operation.
            assert!(!crate::testing::messages(&doc, "c_crit").is_empty());
        }

        // Phase: attaching_a_video_posts_an_inline_video_card.
        {
            let (mut app, mut doc) = app_with_doc();
            let dir = tempfile::tempdir().unwrap();
            let mp4 = dir.path().join("clip.mp4");
            std::fs::write(&mp4, b"\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00moov").unwrap();
            let before = crate::testing::messages(&doc, "c_crit").len();
            app.on_hit(
                Hit::AttachPick(mp4.to_string_lossy().into_owned()),
                &mut ui::widget::Edits::lend(&mut doc),
            );
            settle_attachment_until(&mut app, &mut doc, "c_crit", before + 1);
            let msgs = crate::testing::messages(&doc, "c_crit");
            assert_eq!(msgs.len(), before + 1, "the video posted as one message");
            match &msgs.last().unwrap().attachment {
                Some(model::Attachment::Video {
                    name, blob, size, ..
                }) => {
                    assert_eq!(name, "clip.mp4");
                    assert_eq!(blob.len(), 64, "references a content hash");
                    assert!(app.content.has(blob));
                    assert!(*size > 0, "reads the real file size");
                }
                other => panic!("expected a Video attachment, got {other:?}"),
            }
        }

        // Phase: attaching_a_generic_file_posts_a_portable_file_card.
        {
            let (mut app, mut doc) = app_with_doc();
            let dir = tempfile::tempdir().unwrap();
            let note = dir.path().join("notes.md");
            std::fs::write(&note, b"this is an ordinary attachment").unwrap();
            let before = crate::testing::messages(&doc, "c_crit").len();

            app.on_hit(
                Hit::AttachPick(note.to_string_lossy().into_owned()),
                &mut ui::widget::Edits::lend(&mut doc),
            );
            settle_attachment_until(&mut app, &mut doc, "c_crit", before + 1);
            match &crate::testing::messages(&doc, "c_crit")
                .last()
                .unwrap()
                .attachment
            {
                Some(model::Attachment::File { name, blob, size }) => {
                    assert_eq!(name, "notes.md");
                    assert_eq!(*size, 30);
                    assert!(app.content.has(blob));
                }
                other => panic!("expected a generic File attachment, got {other:?}"),
            }
        }

        // Phase: stale_import_result_does_not_consume_the_accepted_request.
        {
            let (mut app, mut doc) = app_with_doc();
            app.attachment_pending = Some(AttachmentImportPending {
                token: 7,
                channel: app.active_channel.clone(),
            });
            app.attachment_result_tx
                .try_send(AttachmentImportResult::Failed {
                    token: 6,
                    reason: "old worker".into(),
                })
                .unwrap();

            app.settle_attachment_import();
            app.land(&mut doc);
            assert_eq!(
                app.attachment_pending.as_ref().map(|pending| pending.token),
                Some(7)
            );
            assert!(
                app.attachment_status
                    .as_deref()
                    .is_some_and(|status| status.contains("stale"))
            );
        }
    }

    #[test]
    fn switching_to_voice_channel_starts_a_muted_service_join_without_membership_write() {
        let (mut app, mut doc) = app_with_doc();
        let before = doc.save();
        app.on_hit(
            Hit::Channel("c_standup".into()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        app.land(&mut doc);
        app.reload(&doc);
        assert!(app.call.active && app.call.muted && app.call.audio_only);
        assert_eq!(app.call.channel, "c_standup");
        assert_eq!(
            app.native_voice.snapshot.state,
            ui::native_media::State::Preparing
        );
        assert_eq!(doc.save(), before);
    }

    #[test]
    fn expanding_a_call_preserves_verified_streams_without_rewriting_legacy_history() {
        let (mut app, mut doc) = app_with_doc();
        app.on_hit(
            Hit::Channel("c_huddle".into()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        app.land(&mut doc);
        app.reload(&doc);
        service_streams(&mut app, "c_huddle", 3);
        let before = doc.save();
        app.on_hit(Hit::CallExpand, &mut ui::widget::Edits::lend(&mut doc));
        assert!(app.call.expanded);
        assert_eq!(app.voice_in("c_huddle").len(), 3);
        assert_eq!(doc.save(), before);
    }

    #[test]
    fn call_overlay_keeps_the_roster_and_fills_its_surface_at_every_size() {
        // Phase: expanded_overlay_fills_the_surface_at_every_width.
        {
            // The narrow/portrait void regression, exercised through the real app:
            // join the demo huddle, expand, and at each target width the host +
            // remote tiles must cover the whole content band — no large empty
            // region beneath or beside the host.
            use vello::kurbo::Rect;
            let (mut app, mut doc) = app_with_doc();
            app.on_hit(
                Hit::Channel("c_huddle".into()),
                &mut ui::widget::Edits::lend(&mut doc),
            );
            app.land(&mut doc);
            app.reload(&doc);
            service_streams(&mut app, "c_huddle", 3);
            app.on_hit(Hit::CallExpand, &mut ui::widget::Edits::lend(&mut doc));
            assert!(app.call.active && app.call.expanded);

            for &(w, h) in &[(1280.0_f64, 720.0_f64), (800.0, 600.0), (560.0, 720.0)] {
                app.rect = Rect::new(0.0, 0.0, w, h);
                let region = app.overlay_content_region();
                let tiles = app.overlay_tiles();
                assert_eq!(tiles.len(), 4, "w={w}: host + three remotes → four tiles");
                // Every tile inside the region.
                for t in &tiles {
                    assert!(
                        t.x0 >= region.x0 - 0.5
                            && t.x1 <= region.x1 + 0.5
                            && t.y0 >= region.y0 - 0.5
                            && t.y1 <= region.y1 + 0.5,
                        "w={w}: a tile escaped the content region"
                    );
                }
                // Bounding box covers the region (within one gap) in both axes.
                let bbox =
                    tiles
                        .iter()
                        .fold(Rect::new(f64::MAX, f64::MAX, f64::MIN, f64::MIN), |b, r| {
                            Rect::new(
                                b.x0.min(r.x0),
                                b.y0.min(r.y0),
                                b.x1.max(r.x1),
                                b.y1.max(r.y1),
                            )
                        });
                let slack = region.width().min(region.height()) * 0.05 + 17.0;
                assert!(
                    region.x1 - bbox.x1 <= slack && bbox.x0 - region.x0 <= slack,
                    "w={w}: horizontal void — bbox x {:?} vs region x [{:.0},{:.0}]",
                    (bbox.x0, bbox.x1),
                    region.x0,
                    region.x1
                );
                assert!(
                    region.y1 - bbox.y1 <= slack && bbox.y0 - region.y0 <= slack,
                    "w={w}: vertical void below the tiles — bbox y1 {:.0} vs region y1 {:.0}",
                    bbox.y1,
                    region.y1
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

        // Phase: call_resize_samples_keep_content_and_native_chrome_in_one_geometry.
        {
            let (mut app, _doc) = app_with_doc();
            app.call.join("c_huddle");
            render_frame(&mut app, 1200.0, 800.0);
            app.stage_floating_call("floating-call-collapse-000");

            let stable_content = app.call_video_rect();
            let mut prior_height = f64::INFINITY;
            for reveal in [1.0_f32, 0.75, 0.5, 0.25, 0.0] {
                app.call_chrome_motion.pin_for_visual_review(reveal);
                render_frame(&mut app, 1200.0, 800.0);
                let presentation =
                    ui::widget::FloatingSurfaceWidgetExt::floating_surface_presentations(&app)
                        .into_iter()
                        .next()
                        .expect("the detached call publishes its animated native bounds");
                assert_eq!(presentation.content_rect, stable_content);
                assert_eq!(
                    presentation.window_rect,
                    app.huddle_panel().inflate(8.0, 8.0)
                );
                assert!(presentation.window_rect.height() <= prior_height);
                prior_height = presentation.window_rect.height();
            }
            assert_eq!(prior_height, app.huddle_panel().inflate(8.0, 8.0).height());
        }
    }

    #[test]
    fn leaving_a_call_clears_local_service_state_without_rewriting_history() {
        let (mut app, mut doc) = app_with_doc();
        app.on_hit(
            Hit::Channel("c_standup".into()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        app.land(&mut doc);
        app.reload(&doc);
        service_streams(&mut app, "c_standup", 2);
        let before = doc.save();
        assert_eq!(app.voice_in("c_standup").len(), 2);
        app.on_hit(Hit::CallLeave, &mut ui::widget::Edits::lend(&mut doc));
        assert!(!app.call.active && app.call.muted);
        assert!(app.voice_in("c_standup").is_empty());
        assert_eq!(doc.save(), before);
    }

    fn add_channel_asks_then_creates_and_opens_it() {
        let (mut app, mut doc) = app_with_doc();
        let before = app.channels.len();
        app.on_hit(
            Hit::AddChannel {
                workspace: "ws_acme".into(),
                category: "cat_acme_main".into(),
            },
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert_eq!(
            app.channels.len(),
            before,
            "nothing is made before it is named"
        );
        assert!(app.channel_sheet.is_some(), "the channel sheet asks");
        type_str(&mut app, &mut doc, "Launch Plans");
        feed(&mut app, &mut doc, &named_key(NamedKey::Tab));
        type_str(&mut app, &mut doc, "Dates");
        feed(&mut app, &mut doc, &named_key(NamedKey::Enter));
        assert!(app.channel_sheet.is_none(), "Enter creates and closes it");
        let made = app
            .channels
            .iter()
            .find(|c| c.name == "launch-plans")
            .expect("the channel is made under its kept name");
        assert_eq!(made.category, "cat_acme_main");
        assert_eq!(made.topic, "Dates");
        assert_eq!(made.kind, ChannelKind::Text);
        assert_eq!(app.active_channel, made.id, "a new text channel opens");
    }

    #[test]
    fn channels_and_categories_are_created_renamed_and_reordered_from_the_sidebar() {
        add_channel_asks_then_creates_and_opens_it();

        // Phase: inline_rename_writes_through.
        {
            let (mut app, mut doc) = app_with_doc();
            app.begin_rename(MenuTarget::Channel("c_random".into()));
            app.rename_input.select_all();
            type_str(&mut app, &mut doc, "watercooler");
            feed(&mut app, &mut doc, &named_key(NamedKey::Enter));
            let c = app.channels.iter().find(|c| c.id == "c_random").unwrap();
            assert_eq!(c.name, "watercooler");
        }

        // Phase: inline_rename_selection_and_ime_stay_local_until_enter.
        {
            let (mut app, mut doc) = app_with_doc();
            let original = app.channel("c_random").unwrap().name.clone();
            app.begin_rename(MenuTarget::Channel("c_random".into()));

            feed_mods(&mut app, &mut doc, &char_key("a"), mods(true, false));
            assert_eq!(
                app.rename_input.selection_range_public(),
                Some((0, original.len()))
            );
            feed(
                &mut app,
                &mut doc,
                &WindowEvent::Ime(winit::event::Ime::Commit("東京".into())),
            );
            assert_eq!(app.rename_input.value(), "東京");
            assert_eq!(
                crate::testing::channels(&doc)
                    .into_iter()
                    .find(|channel| channel.id == "c_random")
                    .unwrap()
                    .name,
                original,
                "partial rename text is not collaborative state"
            );

            feed(&mut app, &mut doc, &named_key(NamedKey::Enter));
            assert_eq!(app.channel("c_random").unwrap().name, "東京");
        }

        // Phase: category_create_and_collapse_via_the_hit_path.
        {
            let (mut app, mut doc) = app_with_doc();
            let acme_cats = |app: &ChatApp| {
                app.categories
                    .iter()
                    .filter(|c| c.workspace == "ws_acme")
                    .count()
            };
            let before = acme_cats(&app);
            app.open_channel_sheet(crate::channel_sheet::ChannelSheet::new_category("ws_acme"));
            type_str(&mut app, &mut doc, "Research");
            feed(&mut app, &mut doc, &named_key(NamedKey::Enter));
            assert_eq!(acme_cats(&app), before + 1, "a category was created");
            assert!(app.categories.iter().any(|c| c.name == "Research"));
            let collapsed_before = app.category("cat_acme_main").unwrap().collapsed;
            app.on_hit(
                Hit::CategoryToggle("cat_acme_main".into()),
                &mut ui::widget::Edits::lend(&mut doc),
            );
            assert_ne!(
                app.category("cat_acme_main").unwrap().collapsed,
                collapsed_before,
                "the caret toggles the category collapsed state"
            );
        }

        // Phase: reorder_channel_via_menu_action.
        {
            let (mut app, mut doc) = app_with_doc();
            let order =
                |app: &ChatApp, id: &str| app.channels.iter().find(|c| c.id == id).unwrap().order;
            assert!(order(&app, "c_crit") > order(&app, "c_general"));
            activate_context_menu_action(
                &mut app,
                &mut doc,
                MenuTarget::Channel("c_crit".into()),
                MenuAction::MoveUp,
            );
            assert!(
                order(&app, "c_crit") < order(&app, "c_general"),
                "MoveUp swapped order with the sibling above it"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one labelled phase per original scenario, each with its own fresh fixture"
    )]
    fn flaps_and_detail_views_open_reflow_and_unwind() {
        // Phase: toggle_left_flap_flips_state.
        {
            let (mut app, mut doc) = app_with_doc();
            let open = app.left_flap_open();
            app.on_hit(Hit::ToggleLeftFlap, &mut ui::widget::Edits::lend(&mut doc));
            assert_ne!(
                app.left_flap_open(),
                open,
                "toggling the left flap flips it"
            );
        }

        // Phase: flaps_reflow_when_width_changes_on_the_same_instance.
        {
            // Guard against "responsive on first launch, frozen after resize": the
            // flap mode is re-derived from the *current* width every layout pass,
            // never cached from the first frame.
            let (mut app, _doc) = app_with_doc();
            app.rect = vello::kurbo::Rect::new(0.0, 0.0, 1280.0, 720.0);
            app.reconcile_flaps();
            assert!(!app.left_overlay(), "a wide window docks the sidebar");
            // Resize narrow on the SAME instance.
            app.rect = vello::kurbo::Rect::new(0.0, 0.0, 540.0, 720.0);
            app.reconcile_flaps();
            assert!(
                app.left_overlay(),
                "narrowing reflows to one column at a time"
            );
            assert!(app.left_flap.is_push());
            // And widen again.
            app.rect = vello::kurbo::Rect::new(0.0, 0.0, 1280.0, 720.0);
            app.reconcile_flaps();
            assert!(!app.left_overlay(), "widening reflows back to docked");
        }

        // Phase: members_pill_reveals_the_roster_not_a_blank_column.
        {
            let (mut app, mut doc) = app_with_doc();
            app.thread_open = None; // no thread → the flap's other half is members
            app.on_hit(Hit::ToggleRightFlap, &mut ui::widget::Edits::lend(&mut doc));
            render_frame(&mut app, 1280.0, 720.0);
            assert!(
                app.right_flap_open(),
                "the members pill opened the right flap"
            );
            // The members panel painted its own close control (a second
            // ToggleRightFlap hit beyond the always-present header pill), and no
            // thread panel did — i.e. the previously-blank column now has content.
            assert_eq!(
                count_hits(&app, |h| matches!(h, Hit::ToggleRightFlap)),
                2,
                "header pill + members-panel close = two toggle hits (roster shown)"
            );
            assert_eq!(
                count_hits(&app, |h| matches!(h, Hit::CloseThread)),
                0,
                "no thread panel is shown when no thread is open"
            );
        }

        // Phase: narrow_thread_is_a_full_detail_view_with_back_navigation.
        {
            let (mut app, _doc) = app_with_doc();
            app.thread_open = Some("m_concept".into());
            app.rect = vello::kurbo::Rect::new(0.0, 0.0, 390.0, 844.0);
            app.reconcile_flaps();
            app.right_flap.snap(true);

            assert_eq!(
                app.thread_rect(),
                app.rect,
                "a phone-width thread fills the content view rather than floating at desktop width"
            );
            render_frame(&mut app, 390.0, 844.0);
            assert_eq!(
                count_hits(&app, |h| matches!(h, Hit::CloseThread)),
                1,
                "the detail header supplies one Back control"
            );
            assert_eq!(
                count_hits(&app, |h| matches!(h, Hit::ToggleLeftFlap)),
                0,
                "the navigation drawer does not float over pushed thread content"
            );
        }

        // Phase: open_thread_affordance_opens_thread_and_right_flap.
        {
            let (mut app, mut doc) = app_with_doc();
            app.thread_open = None;
            app.right_flap.close();
            app.on_hit(
                Hit::OpenThread("m_concept".into()),
                &mut ui::widget::Edits::lend(&mut doc),
            );
            assert_eq!(app.thread_open.as_deref(), Some("m_concept"));
            assert_eq!(app.focus, Focus::Thread);
            assert!(
                app.right_flap_open(),
                "opening a thread reveals the right flap"
            );
            // Re-hitting the same affordance toggles the thread closed.
            app.on_hit(
                Hit::OpenThread("m_concept".into()),
                &mut ui::widget::Edits::lend(&mut doc),
            );
            assert!(
                app.thread_open.is_none(),
                "re-hitting the affordance closes the thread"
            );
            assert_eq!(app.focus, Focus::Main);
        }

        // Phase: escape_unwinds_the_layer_stack.
        {
            let (mut app, mut doc) = app_with_doc();
            app.emoji_open = true;
            app.thread_open = Some("m_concept".into());
            let mods = Modifiers::default();
            // Top layer first: the emoji picker.
            assert!(app.on_key(
                &Key::Named(NamedKey::Escape),
                mods,
                &mut ui::widget::Edits::lend(&mut doc)
            ));
            assert!(!app.emoji_open, "Escape closes the emoji picker first");
            assert!(
                app.thread_open.is_some(),
                "the thread underneath stays open"
            );
            // Next Escape peels the thread.
            assert!(app.on_key(
                &Key::Named(NamedKey::Escape),
                mods,
                &mut ui::widget::Edits::lend(&mut doc)
            ));
            assert!(
                app.thread_open.is_none(),
                "a second Escape closes the thread"
            );
        }
    }

    fn write_real_png(path: &std::path::Path, w: u32, h: u32) {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([80, 160, 220, 255]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        std::fs::write(path, bytes.into_inner()).unwrap();
    }

    fn settle_attachment_until(
        app: &mut ChatApp,
        doc: &mut crate::testing::Room,
        channel: &str,
        expected_count: usize,
    ) {
        tilo_test_env::wait_until("the attachment worker", || {
            app.settle_attachment_import();
            app.land(doc);
            crate::testing::messages(doc, channel).len() == expected_count
        });
    }

    /// Where this device has no workspace (a browser page), Share opens the
    /// platform's chooser at once; what it, a paste or a drop hands over is
    /// shared from its bytes, one file at a time.
    #[test]
    fn files_from_this_device_are_shared_from_their_bytes() {
        let (mut app, mut doc) = app_with_doc();
        app.active_channel = "c_crit".into();
        app.external_services = false;
        app.on_hit(Hit::Attach, &mut ui::widget::Edits::lend(&mut doc));
        assert!(app.device_pick.is_some(), "the platform's chooser is asked");
        assert!(!app.attach_open, "no workspace listing where there is none");
        app.device_pick = None;

        let before = crate::testing::messages(&doc, "c_crit").len();
        app.device_files
            .push_back(("first.txt".into(), b"one".to_vec()));
        app.device_files
            .push_back(("second.txt".into(), b"two".to_vec()));
        tilo_test_env::wait_until("both chosen files", || {
            app.settle_device_files();
            app.settle_attachment_import();
            app.land(&mut doc);
            crate::testing::messages(&doc, "c_crit").len() == before + 2
        });
        let names: Vec<String> = crate::testing::messages(&doc, "c_crit")
            .iter()
            .rev()
            .take(2)
            .filter_map(|message| message.attachment.as_ref().map(|a| a.name().to_owned()))
            .collect();
        assert_eq!(names, ["second.txt", "first.txt"]);

        app.device_files.push_back(("empty.txt".into(), Vec::new()));
        tilo_test_env::wait_until("the refused file", || {
            app.settle_device_files();
            app.settle_attachment_import();
            app.attachment_pending.is_none() && app.device_files.is_empty()
        });
        assert_eq!(
            app.attachment_status.as_deref(),
            Some("Not shared: the file is empty"),
            "a failure says what happened, never that a workspace is unavailable"
        );
    }

    fn settle_picker_until(app: &mut ChatApp) {
        tilo_test_env::wait_until("the attachment picker worker", || {
            app.settle_attachment_picker();
            app.attachment_picker_pending.is_none()
        });
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one labelled phase per original scenario, each with its own fresh fixture"
    )]
    fn own_messages_are_edited_in_place_and_commit_only_on_save() {
        // Phase: edit_via_menu_writes_through_edit_message.
        {
            let (mut app, mut doc) = app_with_doc();
            let original = app.message("m_me1").unwrap().text.clone();
            let history_before = app.message("m_me1").unwrap().history.len();
            assert!(!app.message("m_me1").unwrap().edited);

            // The right-click menu resolves to this action; invoke it directly.
            activate_context_menu_action(
                &mut app,
                &mut doc,
                MenuTarget::Message("m_me1".into()),
                MenuAction::EditMessage,
            );
            assert!(
                app.editing.is_some(),
                "Edit loads the message into the inline editor"
            );
            assert_eq!(
                app.edit_input.value(),
                original,
                "editor seeded with the current body"
            );

            app.edit_input.select_all();
            type_str(
                &mut app,
                &mut doc,
                "Revised: host tile carries the follow control.",
            );
            feed(&mut app, &mut doc, &named_key(NamedKey::Enter));

            assert!(app.editing.is_none(), "saving closes the editor");
            let m = app.message("m_me1").unwrap();
            assert_eq!(m.text, "Revised: host tile carries the follow control.");
            assert!(m.edited, "the message is flagged edited");
            assert_eq!(
                m.history.len(),
                history_before + 1,
                "history grew by one revision"
            );
            assert_eq!(
                m.history.last().unwrap().text,
                original,
                "history holds the original body"
            );
        }

        // Phase: message_edit_selection_and_ime_commit_only_on_save.
        {
            let (mut app, mut doc) = app_with_doc();
            let original = app.message("m_me1").unwrap().text.clone();
            app.begin_edit("m_me1".into());

            feed_mods(&mut app, &mut doc, &char_key("a"), mods(true, false));
            let (anchor, caret) = app.edit_input.selection();
            assert_eq!((anchor.min(caret), anchor.max(caret)), (0, original.len()));
            feed(
                &mut app,
                &mut doc,
                &WindowEvent::Ime(winit::event::Ime::Preedit("東".into(), Some((0, 3)))),
            );
            assert_eq!(
                app.edit_input.value(),
                original,
                "preedit does not alter the committed input value"
            );
            feed(
                &mut app,
                &mut doc,
                &WindowEvent::Ime(winit::event::Ime::Commit("東京".into())),
            );
            assert_eq!(app.edit_input.value(), "東京");
            assert_eq!(
                crate::testing::messages(&doc, "c_crit")
                    .into_iter()
                    .find(|message| message.id == "m_me1")
                    .unwrap()
                    .text,
                original,
                "partial edits stay local until Save"
            );

            feed(&mut app, &mut doc, &named_key(NamedKey::Enter));
            assert_eq!(app.message("m_me1").unwrap().text, "東京");
        }

        // Phase: clicking_message_editor_places_the_shared_input_caret.
        {
            let (mut app, mut doc) = app_with_doc();
            app.begin_edit("m_me1".into());
            render_frame(&mut app, 1280.0, 720.0);
            let editor = app
                .hits
                .iter()
                .find_map(|(rect, hit)| matches!(hit, Hit::FocusEdit).then_some(*rect))
                .expect("the open editor records its input hit region");

            let x = editor.x0 + 18.0;
            let y = editor.y0 + 14.0;
            feed(&mut app, &mut doc, &cursor_moved(x, y));
            feed(
                &mut app,
                &mut doc,
                &mouse(ElementState::Pressed, MouseButton::Left),
            );
            feed(
                &mut app,
                &mut doc,
                &mouse(ElementState::Released, MouseButton::Left),
            );
            let cursor = app.edit_input.caret();
            assert!(
                cursor < app.edit_input.value().len(),
                "clicking near the left edge moves the caret away from the end"
            );

            type_str(&mut app, &mut doc, "Z");
            assert_eq!(app.edit_input.caret(), cursor + 1);
            assert!(!app.edit_input.value().ends_with('Z'));
        }

        // Phase: arrow_up_in_empty_composer_edits_last_own_message.
        {
            let (mut app, mut doc) = app_with_doc();
            app.focus = Focus::Main;
            app.set_composer_focus();
            assert!(
                app.composer_main.value().is_empty(),
                "composer starts empty"
            );
            feed(&mut app, &mut doc, &named_key(NamedKey::ArrowUp));
            let id = app.editing.clone().expect("ArrowUp opened the editor");
            assert_eq!(id, "m_me2", "loaded the last own top-level message");
            assert_eq!(
                app.edit_input.value(),
                app.message("m_me2").unwrap().text,
                "seeded with its current body"
            );
        }

        // Phase: arrow_up_with_a_draft_does_not_start_editing.
        {
            let (mut app, mut doc) = app_with_doc();
            app.focus = Focus::Main;
            app.set_composer_focus();
            type_str(&mut app, &mut doc, "half a thought");
            feed(&mut app, &mut doc, &named_key(NamedKey::ArrowUp));
            assert!(
                app.editing.is_none(),
                "a non-empty composer suppresses ArrowUp-to-edit"
            );
        }

        // Phase: edit_escape_discards_without_writing.
        {
            let (mut app, mut doc) = app_with_doc();
            let original = app.message("m_me1").unwrap().text.clone();
            app.begin_edit("m_me1".into());
            type_str(&mut app, &mut doc, "Z");
            feed(&mut app, &mut doc, &named_key(NamedKey::Escape));
            assert!(app.editing.is_none(), "Escape closes the editor");
            let m = app.message("m_me1").unwrap();
            assert_eq!(m.text, original, "cancel leaves the message untouched");
            assert!(!m.edited, "cancel does not flag the message edited");
        }

        // Phase: cannot_edit_another_authors_message.
        {
            let (mut app, _doc) = app_with_doc();
            assert_eq!(app.message("m_open").unwrap().author, "u_aria");
            app.begin_edit("m_open".into());
            assert!(
                app.editing.is_none(),
                "a foreign message never enters the editor"
            );
        }

        // Phase: edit_history_popover_opens_and_dismisses.
        {
            let (mut app, mut doc) = app_with_doc();
            assert!(
                app.message("m_sora").unwrap().edited,
                "m_sora is an edited message"
            );
            app.on_hit(
                Hit::EditHistory {
                    msg: "m_sora".into(),
                    anchor: Point::ZERO,
                },
                &mut ui::widget::Edits::lend(&mut doc),
            );
            assert_eq!(
                app.history_open.as_deref(),
                Some("m_sora"),
                "the history popover opened"
            );
            app.on_hit(Hit::Dismiss, &mut ui::widget::Edits::lend(&mut doc));
            assert!(app.history_open.is_none(), "the dismiss scrim closes it");
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the floating-surface regression deliberately follows the complete app-to-shell presentation lifecycle"
    )]
    fn call_tiles_and_controls_stay_honest_and_live_when_detached() {
        // Phase: host_call_tile_has_no_fabricated_video_frame.
        {
            // The host video surface must be empty until a real capture feeds it —
            // never a synthesized gradient "face".
            let app = ChatApp::new_ephemeral();
            assert!(
                app.video.latest_frame().is_none(),
                "no frame is fabricated at construction"
            );
        }

        // Phase: detached_call_keeps_live_controls_in_the_shell_presentation_pass.
        {
            use ui::Widget;
            use ui::floating_surface::Attachment;
            use ui::presence::PresenceCtx;
            use ui::text_render::TextRenderer;
            use ui::theme::Theme;
            use vello::Scene;
            use vello::kurbo::{Affine, Rect};

            let (mut app, mut doc) = app_with_doc();
            app.call.join("c_huddle");
            render_frame(&mut app, 1200.0, 800.0);
            ui::widget::FloatingSurfaceWidgetExt::set_floating_surface_bounds(
                &mut app,
                Rect::new(0.0, 0.0, 1600.0, 900.0),
            );

            let handle = app
                .hits
                .iter()
                .find_map(|(rect, hit)| matches!(hit, Hit::CallDragHandle).then_some(*rect))
                .expect("the attached live call exposes its drag handle");
            let pointer_origin = handle.center();
            let surface_origin = app.call_video_rect();
            feed(
                &mut app,
                &mut doc,
                &cursor_moved(pointer_origin.x, pointer_origin.y),
            );
            feed(
                &mut app,
                &mut doc,
                &mouse(ElementState::Pressed, MouseButton::Left),
            );
            assert!(Widget::wants_pointer_capture(&app));

            let outside = Point::new(1300.0, pointer_origin.y + 40.0);
            feed(&mut app, &mut doc, &cursor_moved(outside.x, outside.y));
            assert!(matches!(
                app.call.placement.attachment(),
                Attachment::Desktop { .. }
            ));
            assert_eq!(
                app.call_video_rect(),
                surface_origin + (outside - pointer_origin),
                "the transfer frame preserves the live surface geometry"
            );
            assert!(
                ui::widget::FloatingSurfaceWidgetExt::take_floating_surface_request(&mut app)
                    .is_some(),
                "the compositor receives the one-shot adoption request"
            );

            // Preparing a native window is not yet proof of pixels. Keep the
            // clipped app copy alive until the compositor acknowledges the first
            // committed floating buffer, so there is no transparent transfer
            // frame even when GPU/window setup takes a beat.
            render_frame(&mut app, 1200.0, 800.0);
            assert_eq!(
                count_hits(&app, |hit| matches!(hit, Hit::CallDragHandle)),
                1
            );
            let prepared_content = app.call_video_rect();
            let prepared_window = app.huddle_panel().inflate(8.0, 8.0);
            ui::widget::FloatingSurfaceWidgetExt::on_floating_surface_host_message(
                &mut app,
                &ui::floating_surface::FloatingSurfaceHostMessage::PrepareSurface {
                    id: "chat-call:c_huddle".into(),
                    token: "0123456789abcdef0123456789abcdef".into(),
                    rect: ui::floating_surface::protocol_rect_array(prepared_window),
                    content_rect: ui::floating_surface::protocol_rect_array(prepared_content),
                    transfer_covered: true,
                    coordinate_offset: [0.0, 0.0],
                },
            );
            assert!(!app.call.placement.is_desktop_adopted());
            let mut scene = Scene::new();
            let mut text_renderer = TextRenderer::new();
            ui::widget::FloatingSurfaceWidgetExt::draw_floating_surfaces(
                &mut app,
                &mut scene,
                Affine::IDENTITY,
                &mut text_renderer,
                &Theme::dark(),
                &PresenceCtx::empty(),
            );
            assert!(!scene.encoding().is_empty());

            ui::widget::FloatingSurfaceWidgetExt::on_floating_surface_host_message(
                &mut app,
                &ui::floating_surface::FloatingSurfaceHostMessage::Adopted {
                    id: "chat-call:c_huddle".into(),
                    rect: ui::floating_surface::protocol_rect_array(prepared_content),
                },
            );
            assert!(app.call.placement.is_desktop_adopted());
            render_frame(&mut app, 1200.0, 800.0);
            assert_eq!(
                count_hits(&app, |hit| matches!(hit, Hit::CallDragHandle)),
                0
            );
            let mut scene = Scene::new();
            ui::widget::FloatingSurfaceWidgetExt::draw_floating_surfaces(
                &mut app,
                &mut scene,
                Affine::IDENTITY,
                &mut text_renderer,
                &Theme::dark(),
                &PresenceCtx::empty(),
            );
            assert_eq!(
                count_hits(&app, |hit| matches!(hit, Hit::CallDragHandle)),
                1
            );

            // Drag back well inside Chat. The runtime sees `returning_to_app` in
            // the presentation and asks the shell to suspend its native surface;
            // until that acknowledgement arrives only the shell pass may paint.
            let returned_pointer = Point::new(980.0, pointer_origin.y + 84.0);
            feed(
                &mut app,
                &mut doc,
                &cursor_moved(returned_pointer.x, returned_pointer.y),
            );
            assert!(app.call.placement.is_return_pending());
            let final_shell_rect = app.call_video_rect();
            let presentations =
                ui::widget::FloatingSurfaceWidgetExt::floating_surface_presentations(&app);
            assert_eq!(presentations.len(), 1);
            assert!(presentations[0].returning_to_app);

            ui::widget::FloatingSurfaceWidgetExt::on_floating_surface_host_message(
                &mut app,
                &ui::floating_surface::FloatingSurfaceHostMessage::Suspended {
                    id: "chat-call:c_huddle".into(),
                },
            );
            assert!(matches!(
                app.call.placement.attachment(),
                Attachment::App { .. }
            ));
            assert_eq!(
                app.call_video_rect(),
                final_shell_rect,
                "the first app-owned call frame must exactly cover the final shell frame"
            );
            assert!(
                Widget::wants_pointer_capture(&app),
                "the held drag survives shell suspension"
            );

            let continued_pointer = returned_pointer + vello::kurbo::Vec2::new(-18.0, 11.0);
            feed(
                &mut app,
                &mut doc,
                &cursor_moved(continued_pointer.x, continued_pointer.y),
            );
            assert_eq!(
                app.call_video_rect(),
                final_shell_rect + (continued_pointer - returned_pointer),
                "the first primary-window motion continues the same drag"
            );

            // Release is still delivered after both ownership boundaries.
            feed(
                &mut app,
                &mut doc,
                &mouse(ElementState::Released, MouseButton::Left),
            );
            assert!(!Widget::wants_pointer_capture(&app));
        }

        // Phase: call_controls_track_hover_and_clear_it_on_departure.
        {
            let (mut app, mut doc) = app_with_doc();
            app.call.join("c_huddle");
            render_frame(&mut app, 1200.0, 800.0);
            let leave = app
                .hits
                .iter()
                .find_map(|(rect, hit)| matches!(hit, Hit::CallLeave).then_some(*rect))
                .expect("the live huddle exposes Leave");

            feed(
                &mut app,
                &mut doc,
                &cursor_moved(leave.center().x, leave.center().y),
            );
            assert!(matches!(app.call_hovered, Some(Hit::CallLeave)));

            feed(&mut app, &mut doc, &cursor_moved(2.0, 2.0));
            assert!(app.call_hovered.is_none());
        }
    }

    // --- draw + input harness -------------------------------------------
    //
    // The chat surface computes its regions from `self.rect` and records every
    // clickable area into `self.hits` during `draw`, so a single draw pass at a
    // known size fully populates the hit list a real click would resolve
    // against — no GPU or window needed (identical to the offscreen `--shots`
    // path). These helpers drive that and the raw winit input path.

    fn render_frame(app: &mut ChatApp, w: f64, h: f64) {
        use ui::Widget;
        use ui::presence::PresenceCtx;
        use ui::text_render::TextRenderer;
        use ui::theme::Theme;
        use vello::Scene;
        use vello::kurbo::Affine;
        app.rect = vello::kurbo::Rect::new(0.0, 0.0, w, h);
        let theme = Theme::dark();
        let presence = PresenceCtx::empty();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        app.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    }

    #[test]
    fn private_image_pastes_never_enter_the_room_upload_queue() {
        for scenario in ["TaskGoalBeforeSetup", "agent-task", "person"] {
            let (mut app, mut doc) = app_with_doc();
            if scenario == "person" {
                app.direct_person = Some("certified-person-key".into());
            } else {
                app.stage_scenario(scenario);
            }
            app.queue_pasted_images(vec![ui::clipboard::ClipboardImage {
                mime: "image/png",
                bytes: b"private image".to_vec(),
            }]);
            assert!(app.device_files.is_empty());
            assert!(app.attachment_status.is_some());
            app.on_hit(Hit::CurrentContext, &mut ui::widget::Edits::lend(&mut doc));
            app.settle_device_files();
            assert!(app.attachment_pending.is_none());
        }
        let (mut app, _) = app_with_doc();
        app.queue_pasted_images(vec![ui::clipboard::ClipboardImage {
            mime: "image/png",
            bytes: b"room image".to_vec(),
        }]);
        assert_eq!(app.device_files.len(), 1);
    }

    #[test]
    fn tasks_remain_reachable_without_a_workspace_rail() {
        let (mut app, mut doc) = app_with_doc();
        // A single workspace has no Inbox destination, but still needs Tasks.
        let context = app.context_ws.clone();
        app.channels.retain(|channel| channel.workspace == context);
        app.workspaces.retain(|workspace| workspace.id == context);
        app.left_flap.snap(true);
        render_frame(&mut app, 1280.0, 800.0);
        assert!(app.hits.iter().any(|(_, hit)| matches!(hit, Hit::Tasks)));
        app.on_hit(Hit::Tasks, &mut ui::widget::Edits::lend(&mut doc));
        assert!(app.task_goal.is_some());
        app.left_flap.snap(true);
        render_frame(&mut app, 1280.0, 800.0);
        assert!(app.hits.iter().any(|(_, hit)| matches!(hit, Hit::NewTask)));
        assert!(
            app.hits
                .iter()
                .any(|(_, hit)| matches!(hit, Hit::CurrentContext))
        );
        app.on_hit(Hit::CurrentContext, &mut ui::widget::Edits::lend(&mut doc));
        assert!(app.task_goal.is_none());
        assert!(app.direct_agent.is_none());
        assert_eq!(app.active_ws, context);
    }

    #[test]
    fn inbox_leaves_private_task_navigation() {
        for scenario in ["TaskGoalBeforeSetup", "agent-task"] {
            let (mut app, mut doc) = app_with_doc();
            app.stage_scenario(scenario);
            app.left_flap.snap(true);
            render_frame(&mut app, 1280.0, 800.0);
            assert!(app.hits.iter().any(|(_, hit)| matches!(hit, Hit::Inbox)));
            app.on_hit(Hit::Inbox, &mut ui::widget::Edits::lend(&mut doc));
            assert!(app.inbox_open);
            assert!(app.task_goal.is_none());
            assert!(app.direct_agent.is_none());
        }
    }

    #[test]
    fn private_headers_never_publish_channel_calls_or_invites() {
        for scenario in ["TaskGoalBeforeSetup", "agent-task", "Subtask"] {
            let (mut app, mut doc) = app_with_doc();
            app.stage_scenario(scenario);
            render_frame(&mut app, 1280.0, 800.0);
            assert!(app.task_goal.is_some() || app.direct_agent.is_some());
            assert!(
                !app.hits
                    .iter()
                    .any(|(_, hit)| matches!(hit, Hit::StartCall | Hit::ToggleRightFlap))
            );
            assert!(
                app.hits
                    .iter()
                    .any(|(_, hit)| matches!(hit, Hit::AgentWindow))
            );
            let controls = app.published_rows();
            assert!(!controls.iter().any(|row| row.id() == "chat.start-call"));
            app.on_hit(Hit::CurrentContext, &mut ui::widget::Edits::lend(&mut doc));
            app.left_flap.snap(true);
            render_frame(&mut app, 1280.0, 800.0);
            assert!(app.task_goal.is_none() && app.direct_agent.is_none());
            assert!(app.has_other_collaborator());
            assert!(
                app.hits
                    .iter()
                    .any(|(_, hit)| matches!(hit, Hit::StartCall))
            );
            assert!(
                app.hits
                    .iter()
                    .any(|(_, hit)| matches!(hit, Hit::ToggleRightFlap))
            );
            assert!(
                app.hits
                    .iter()
                    .any(|(_, hit)| matches!(hit, Hit::VoiceChannel(_)))
            );
        }
    }

    #[test]
    fn private_person_header_publishes_private_identity_without_channel_controls() {
        let (mut app, _doc) = app_with_doc();
        app.direct_person = Some("certified-person-key".into());
        app.sync_person_draft();
        render_frame(&mut app, 1280.0, 800.0);
        assert!(app.has_other_collaborator());
        assert!(
            !app.hits
                .iter()
                .any(|(_, hit)| matches!(hit, Hit::StartCall | Hit::ToggleRightFlap))
        );
        assert!(
            app.hits
                .iter()
                .any(|(_, hit)| matches!(hit, Hit::AgentWindow))
        );
        let semantics = ui::Widget::accessibility(&app).unwrap();
        assert!(
            semantics
                .children
                .iter()
                .any(|node| node.description.as_deref() == Some("Private conversation"))
        );
    }

    #[test]
    fn narrow_task_tree_scrolls_to_last_child_without_clipped_hit_targets() {
        let (mut app, mut doc) = app_with_doc();
        app.stage_scenario("Subtask");
        let workspace = app.active_ws.clone();
        let view = app.agent_tasks.stage_capture(&workspace, "scribe");
        let parent = view.current().unwrap().clone();
        for index in 0..20 {
            let mut child = parent.clone();
            child.task_id = format!("task:child-{index}");
            child.title = format!("Example child {index}");
            child.child_order = Some(index);
            child.parent_task_id = Some(parent.task_id.clone());
            view.tasks.push(child);
        }
        app.expanded_tasks.insert(parent.task_id);
        app.left_flap.snap(true);
        render_frame(&mut app, 420.0, 680.0);
        assert!(app.task_navigation.overflows_vertically());
        assert!(
            !app.hits
                .iter()
                .any(|(_, hit)| matches!(hit, Hit::VoiceChannel(_)))
        );
        let viewport = app.task_navigation.content_rect();
        assert!(viewport.y1 <= app.rect.y1);
        let center = viewport.center();
        drive_retained_control(
            &mut app,
            &cursor_moved(center.x, center.y),
            1.0,
            Modifiers::default(),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        drive_retained_control(
            &mut app,
            &wheel(-100.0),
            1.0,
            Modifiers::default(),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert!(app.task_navigation.offset().y > 0.0);
        render_frame(&mut app, 420.0, 680.0);
        let rows = app
            .hits
            .iter()
            .filter_map(|(rect, hit)| match hit {
                Hit::AgentTask(task) => Some((rect, task)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(rows.iter().any(|(_, task)| task.task_id == "task:child-19"));
        for (rect, _) in rows {
            assert!(rect.y0 >= viewport.y0 && rect.y1 <= viewport.y1);
            assert!(rect.x0 >= viewport.x0 && rect.x1 <= viewport.x1);
            assert!(rect.height() > 0.0);
        }
    }

    #[test]
    fn narrow_private_goal_editor_and_controls_stay_inside_the_viewport() {
        for goal in ["A private goal", "A private goal\nwith a second line"] {
            let (mut app, doc) = app_with_doc();
            app.begin_task_goal(None);
            app.agent_tasks.stage_missing_model_capture();
            app.answer_effect(crate::effects::ChatEffect::SetTaskGoal { text: goal.into() })
                .unwrap();
            app.composer_main.sync(&doc);
            app.set_composer_focus();
            render_frame(&mut app, 420.0, 680.0);
            assert_eq!(app.composer_main.private_plain().as_deref(), Some(goal));
            let input = app.composer_main.rect();
            let retry = app
                .hits
                .iter()
                .find_map(|(rect, hit)| matches!(hit, Hit::AgentSetup).then_some(*rect))
                .unwrap();
            let send = app
                .hits
                .iter()
                .find_map(|(rect, hit)| matches!(hit, Hit::Send).then_some(*rect))
                .unwrap();
            assert!(retry.y1 <= input.y0);
            assert!(input.y1 <= app.rect.y1 && input.y0 >= app.header_rect().y1);
            assert!(send.x0 >= input.x1 && send.x1 <= app.rect.x1);
            assert!(!crate::projection::has_draft(&doc, &app.active_channel));
        }
    }

    #[test]
    fn new_task_accepts_private_goal_before_any_worker_or_model_exists() {
        let (mut app, mut doc) = app_with_doc();
        app.room_agents.clear();
        app.answer_effect(crate::effects::ChatEffect::NewTask)
            .unwrap();
        app.agent_tasks.stage_missing_model_capture();
        app.composer_main.sync(&doc);
        app.set_composer_focus();
        type_str(
            &mut app,
            &mut doc,
            "Keep the actual goal private before model setup.",
        );
        app.composer_main.select(5, 15);
        let selection = app.composer_main.selection();
        let channel = app.active_channel.clone();
        let text = app.composer_main.private_plain().unwrap();
        assert!(app.direct_agent.is_none());
        assert!(app.direct_person.is_none());
        assert!(app.task_goal.is_some());
        assert!(!crate::projection::has_draft(&doc, &channel));
        assert!(app.composer_presence().is_none());
        assert!(app.save_task_goal());
        assert!(app.task_goal.as_ref().unwrap().saving);
        assert!(app.task_goal.as_ref().unwrap().error.is_none());
        assert!(app.agent_tasks.all_tasks().is_empty());
        assert!(app.direct_agent.is_none());
        app.reload(&doc);
        render_frame(&mut app, 900.0, 800.0);
        assert_eq!(
            app.composer_main.private_plain().as_deref(),
            Some(text.as_str())
        );
        assert_eq!(app.composer_main.selection(), selection);
        assert!(
            app.hits
                .iter()
                .any(|(_, hit)| matches!(hit, Hit::AgentSetup))
        );
        assert!(
            !app.hits
                .iter()
                .any(|(_, hit)| matches!(hit, Hit::AgentStart))
        );
        assert!(app.messages.is_empty());
        assert!(!crate::projection::has_draft(&doc, &channel));
    }

    #[test]
    fn private_subtask_composer_does_not_create_illustrative_or_empty_children() {
        let (mut app, mut doc) = app_with_doc();
        app.stage_scenario("Subtask");
        let parent = app
            .agent_tasks
            .all_tasks()
            .into_iter()
            .find(|task| task.parent_task_id.is_none())
            .unwrap()
            .task_id
            .clone();
        let before = app.agent_tasks.all_tasks().len();
        app.answer_effect(crate::effects::ChatEffect::NewSubtaskGoal {
            task: parent.clone(),
        })
        .unwrap();
        app.composer_main.sync(&doc);
        app.set_composer_focus();
        type_str(
            &mut app,
            &mut doc,
            "Private child instruction, not its public label.",
        );
        let child_key = app.active_channel.clone();
        let child_text = app.composer_main.private_plain().unwrap();
        assert_eq!(
            app.task_goal.as_ref().unwrap().parent.as_deref(),
            Some(parent.as_str())
        );
        assert_eq!(app.agent_tasks.all_tasks().len(), before);
        assert!(app.save_task_goal());
        assert!(app.task_goal.as_ref().unwrap().saving);
        assert_eq!(app.agent_tasks.all_tasks().len(), before);
        app.answer_effect(crate::effects::ChatEffect::NewTask)
            .unwrap();
        assert!(
            app.composer_main
                .private_plain()
                .unwrap_or_default()
                .is_empty()
        );
        app.answer_effect(crate::effects::ChatEffect::NewSubtaskGoal { task: parent })
            .unwrap();
        app.composer_main.sync(&doc);
        assert_eq!(app.active_channel, child_key);
        assert_eq!(
            app.composer_main.private_plain().as_deref(),
            Some(child_text.as_str())
        );
        assert!(!crate::projection::has_draft(&doc, &child_key));
        assert_eq!(app.agent_tasks.all_tasks().len(), before);
    }

    #[test]
    fn task_actions_and_render_keep_shared_compute_conversations_and_carets_separate() {
        let (mut app, mut doc) = app_with_doc();
        app.stage_scenario("Subtask");
        app.composer_main.sync(&doc);
        app.set_composer_focus();
        type_str(&mut app, &mut doc, "Only the parent may see this draft.");
        app.composer_main.select(5, 12);
        let selection = app.composer_main.selection();
        let parent_channel = app.active_channel.clone();
        app.answer_effect(crate::effects::ChatEffect::OpenTaskConversation {
            workspace: app.active_ws.clone(),
            agent: "scribe".into(),
            task: "task:review-child".into(),
            run_epoch: 0,
        })
        .unwrap();
        assert_ne!(app.active_channel, parent_channel);
        assert_eq!(
            app.agent_draft_route().as_deref(),
            Some("task:task:review-child:0")
        );
        assert_eq!(app.composer_main.private_plain().unwrap_or_default(), "");
        render_frame(&mut app, 1280.0, 800.0);
        assert!(
            app.messages
                .iter()
                .all(|message| !message.text.contains("Keep the existing open action"))
        );
        app.answer_effect(crate::effects::ChatEffect::OpenTaskConversation {
            workspace: app.active_ws.clone(),
            agent: "scribe".into(),
            task: "task:review".into(),
            run_epoch: 7,
        })
        .unwrap();
        assert_eq!(app.active_channel, parent_channel);
        assert_eq!(
            app.composer_main.private_plain().as_deref(),
            Some("Only the parent may see this draft.")
        );
        assert_eq!(app.composer_main.selection(), selection);
        render_frame(&mut app, 1280.0, 800.0);
        assert_eq!(
            app.header_identity_text().name,
            "Example: Review search actions"
        );
        assert!(
            app.published_rows()
                .iter()
                .any(|row| row.id() == "chat.task:task:review-child:0")
        );
    }

    #[test]
    fn task_hierarchy_and_filters_use_live_actions_and_production_rows() {
        let (mut app, _doc) = app_with_doc();
        app.stage_scenario("Subtask");
        render_frame(&mut app, 1280.0, 800.0);
        assert!(app.hits.iter().any(
            |(_, hit)| matches!(hit, Hit::AgentTask(task) if task.task_id == "task:review-child")
        ));
        app.answer_effect(crate::effects::ChatEffect::ToggleTaskChildren {
            task: "task:review".into(),
        })
        .unwrap();
        render_frame(&mut app, 1280.0, 800.0);
        assert!(!app.hits.iter().any(
            |(_, hit)| matches!(hit, Hit::AgentTask(task) if task.task_id == "task:review-child")
        ));
        app.answer_effect(crate::effects::ChatEffect::FilterTasks {
            filter: "completed".into(),
        })
        .unwrap();
        render_frame(&mut app, 1280.0, 800.0);
        assert!(
            !app.hits
                .iter()
                .any(|(_, hit)| matches!(hit, Hit::AgentTask(_)))
        );
        app.answer_effect(crate::effects::ChatEffect::FilterTasks {
            filter: "all".into(),
        })
        .unwrap();
        app.answer_effect(crate::effects::ChatEffect::CreateSubtask {
            task: "task:review".into(),
            title: "Explicit public label".into(),
        })
        .unwrap();
        assert!(
            app.published_rows()
                .iter()
                .any(|row| row.id() == "chat.tasks")
        );
    }

    #[test]
    fn stop_pending_paints_stopping_without_claiming_external_undo() {
        let (mut app, _doc) = app_with_doc();
        app.stage_scenario("RunningSteering");
        assert!(app.stop_agent_task());
        render_frame(&mut app, 420.0, 680.0);
        let lines = crate::render_chat::agent_task_status_lines(app.direct_agent_tasks().unwrap());
        let receipt = lines.receipt.unwrap();
        assert!(receipt.contains("Stopping"));
        assert!(receipt.contains("not undone"));
        assert!(
            !app.hits
                .iter()
                .any(|(_, hit)| matches!(hit, Hit::AgentStop))
        );
    }

    #[test]
    fn agent_task_conversation_normal_and_compact_keep_exact_control_and_private_editor() {
        for (scenario, width) in [("agent-task", 1280.0), ("agent-task-compact", 420.0)] {
            let (mut app, mut doc) = app_with_doc();
            app.stage_scenario(scenario);
            app.composer_main.sync(&doc);
            app.focus = Focus::Main;
            app.set_composer_focus();
            type_str(&mut app, &mut doc, "Keep this private clarification.");
            app.composer_main.select(5, 17);
            let selection = app.composer_main.selection();
            let route = app.agent_draft_route().unwrap();
            assert_eq!(route, "task:task:review:7");
            render_frame(&mut app, width, 680.0);
            assert!(
                app.hits
                    .iter()
                    .any(|(_, hit)| matches!(hit, Hit::AgentStop))
            );
            assert!(
                !app.hits
                    .iter()
                    .any(|(_, hit)| matches!(hit, Hit::AgentStart)),
                "an active run cannot be approved again"
            );
            assert!(app.hits.iter().any(|(_, hit)| matches!(hit, Hit::Send)));
            assert_eq!(
                app.header_identity_text().name,
                "Example: Review search actions"
            );
            assert_eq!(
                app.messages.len(),
                5,
                "private reports and directions reach the real timeline"
            );
            assert!(
                app.messages
                    .iter()
                    .any(|message| message.text.contains("Example action output"))
            );
            assert!(
                app.messages
                    .iter()
                    .any(|message| message.text.contains("Example resource output"))
            );
            assert_eq!(
                app.user_name("agent:scribe"),
                "Example: Review search actions"
            );
            let lines =
                crate::render_chat::agent_task_status_lines(app.direct_agent_tasks().unwrap());
            assert!(lines.primary.contains("Working"));
            assert!(!lines.primary.contains("task:review"));
            assert_eq!(
                lines.receipt,
                Some(crate::i18n::t("chat-agent-direction-waiting"))
            );
            let semantics = ui::Widget::accessibility(&app).unwrap();
            let heading = semantics
                .children
                .iter()
                .find(|node| node.name.as_deref() == Some("Example: Review search actions"))
                .expect("the selected task has an accessible heading");
            assert!(
                heading
                    .description
                    .as_deref()
                    .unwrap()
                    .contains(&crate::i18n::t("chat-agent-direction-waiting"))
            );
            assert!(!crate::projection::has_draft(&doc, &app.active_channel));

            let channel = app.active_channel.clone();
            let mut surface =
                crate::agent_conversation_surface::AgentConversationSurface::from_attached(
                    &mut app, "scribe",
                );
            assert_eq!(
                surface.view.agent_draft_route().as_deref(),
                Some(route.as_str())
            );
            assert_eq!(
                surface.view.composer_main.value(),
                "Keep this private clarification."
            );
            assert_eq!(surface.view.composer_main.selection(), selection);
            assert_eq!(surface.request().title, "Example: Review search actions");
            assert_eq!(
                surface
                    .view
                    .direct_agent_tasks()
                    .unwrap()
                    .selected_identity(),
                Some(("task:review", 7))
            );
            surface.move_editor_to_parent(&mut app);
            assert_eq!(app.agent_draft_route().as_deref(), Some(route.as_str()));
            assert_eq!(app.composer_main.selection(), selection);
            assert_eq!(app.active_channel, channel);
            assert_eq!(
                app.direct_agent_tasks()
                    .unwrap()
                    .current()
                    .unwrap()
                    .run_epoch,
                Some(7)
            );
        }
    }

    #[test]
    fn private_reports_survive_cursor_editor_adoption_and_surface_round_trip() {
        let (mut app, doc) = app_with_doc();
        app.stage_scenario("agent-task");
        app.composer_main.sync(&doc);
        render_frame(&mut app, 1280.0, 680.0);
        let before = app
            .messages
            .iter()
            .map(|message| {
                (
                    message.id.clone(),
                    message.author.clone(),
                    message.text.clone(),
                    message.ts,
                )
            })
            .collect::<Vec<_>>();
        assert!(
            before
                .iter()
                .any(|(_, author, _, _)| author == "task-report:task:review")
        );
        let key = crate::composer::ConversationKey::Agent {
            workspace: app.active_ws.clone(),
            route: app.agent_draft_route().unwrap(),
        };
        let editor = ui::private_draft::Editor {
            text: "Cursor clarification".into(),
            anchor: 8,
            caret: 2,
            preedit: "候補".into(),
            preedit_cursor: Some((0, 3)),
        };
        app.composer_main
            .adopt_private_editor(&key, &editor)
            .unwrap();
        let mut surface =
            crate::agent_conversation_surface::AgentConversationSurface::from_attached(
                &mut app, "scribe",
            );
        render_frame(&mut surface.view, 420.0, 680.0);
        let floated = surface
            .view
            .messages
            .iter()
            .map(|message| {
                (
                    message.id.clone(),
                    message.author.clone(),
                    message.text.clone(),
                    message.ts,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(before, floated);
        assert_eq!(surface.view.composer_main.selection(), (8, 2));
        surface.move_editor_to_parent(&mut app);
        render_frame(&mut app, 1280.0, 680.0);
        let restored = app
            .messages
            .iter()
            .map(|message| {
                (
                    message.id.clone(),
                    message.author.clone(),
                    message.text.clone(),
                    message.ts,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(before, restored);
        assert_eq!(app.composer_main.value(), "Cursor clarification");
        assert_eq!(app.composer_main.selection(), (8, 2));
        assert!(!crate::projection::has_draft(&doc, &app.active_channel));
        // The shared room projection has neither private report ids nor bodies.
        let shared = app.held_messages();
        assert!(shared.iter().all(|(id, message)| {
            !id.to_string().starts_with("task-report:")
                && !before.iter().any(|(_, author, body, _)| {
                    author.starts_with("task-report:") && body == &message.text
                })
        }));
    }

    #[test]
    fn queued_task_conversation_draws_explicit_start_without_stop() {
        for (scenario, width) in [
            ("agent-task-queued", 1280.0),
            ("agent-task-queued-compact", 420.0),
        ] {
            let (mut app, mut doc) = app_with_doc();
            app.stage_scenario(scenario);
            app.composer_main.sync(&doc);
            render_frame(&mut app, width, 680.0);
            let start = app
                .hits
                .iter()
                .find(|(_, hit)| matches!(hit, Hit::AgentStart))
                .expect("the production paint exposes explicit Start task");
            assert!(start.0.x0 >= 0.0 && start.0.x1 <= width);
            assert!(
                !app.hits
                    .iter()
                    .any(|(_, hit)| matches!(hit, Hit::AgentStop))
            );
            assert_eq!(app.messages.len(), 1);
            assert_eq!(
                app.direct_agent_tasks()
                    .unwrap()
                    .current()
                    .unwrap()
                    .run_epoch,
                Some(0)
            );
            app.focus = Focus::Main;
            app.set_composer_focus();
            type_str(&mut app, &mut doc, "An unsent new task");
            assert!(!crate::projection::has_draft(&doc, &app.active_channel));
            assert_eq!(app.composer_main.value(), "An unsent new task");
        }
    }

    #[test]
    fn finished_task_followup_keeps_its_private_editor_but_queues_a_new_task() {
        let (mut app, mut doc) = app_with_doc();
        app.stage_scenario("RunningSteering");
        render_frame(&mut app, 420.0, 680.0);
        app.set_composer_focus();
        type_str(&mut app, &mut doc, "Keep this follow-up private.");
        app.composer_main.select(5, 12);
        let route = app.agent_draft_route();
        let text = app.composer_main.value();
        let selection = app.composer_main.selection();
        app.agent_tasks
            .stage_answer_capture(&app.active_ws, "scribe");
        app.sync_agent_route();
        render_frame(&mut app, 420.0, 680.0);
        assert_eq!(
            app.direct_agent_intent(),
            crate::agent_tasks::AgentComposerIntent::Queue
        );
        assert!(app.main_composer_can_edit());
        assert_eq!(app.agent_draft_route(), route);
        assert_eq!(app.composer_main.value(), text);
        assert_eq!(app.composer_main.selection(), selection);
        assert!(!app.direct_agent_tasks().unwrap().can_steer());
        assert!(
            !app.hits
                .iter()
                .any(|(_, hit)| matches!(hit, Hit::AgentStop))
        );
        assert!(!crate::projection::has_draft(&doc, &app.active_channel));
    }

    #[test]
    fn task_setup_pending_is_passive_until_the_service_reports_missing_model() {
        let (mut app, _doc) = app_with_doc();
        app.begin_task_goal(None);
        render_frame(&mut app, 420.0, 680.0);
        assert!(
            !app.hits
                .iter()
                .any(|(_, hit)| matches!(hit, Hit::AgentSetup))
        );
        app.agent_tasks.stage_missing_model_capture();
        render_frame(&mut app, 420.0, 680.0);
        assert!(
            app.hits
                .iter()
                .any(|(_, hit)| matches!(hit, Hit::AgentSetup))
        );
        assert_eq!(
            app.agent_tasks.setup_label(),
            crate::i18n::t("chat-task-setup")
        );
    }

    fn stage_agent_work(app: &mut ChatApp, doc: &mut crate::testing::Room) {
        for (author, invoke) in crate::resource::agent_work_steps() {
            crate::testing::apply_as(doc, author, invoke).unwrap();
        }
        for (author, invoke) in crate::resource::agent_work_part_steps() {
            crate::testing::apply_part_as(doc, author, invoke).unwrap();
        }
        app.land(doc);
        app.stage_scenario("agent-work");
    }

    #[test]
    fn sidebar_accessibility_focus_retires_the_previous_keyboard_and_drawer_owner() {
        use ui::Widget;
        use ui::a11y::Action;
        use ui::headless::FrameHarness;
        let (app, doc) = app_with_doc();
        let mut state = ui::State::new();
        assert!(state.adopt_doc(doc.chat));
        let mut driver = FrameHarness::for_root(state, app, (1800, 900), ui::Theme::dark());
        let people = driver
            .a11y_node_named("Show people or thread")
            .expect("the actual Chat People control");
        driver
            .perform_a11y_action(people, &Action::Activate)
            .unwrap();
        assert!(
            driver.wait_until(ui::time::Duration::from_secs(1), |driver| {
                driver
                    .root::<ChatApp>()
                    .right_flap
                    .resize_accessibility()
                    .is_some()
            })
        );
        let people = driver.a11y_node_named("Show people or thread").unwrap();
        driver.perform_a11y_action(people, &Action::Focus).unwrap();
        assert!(driver.root::<ChatApp>().keyboard_target.is_some());
        let sidebar = driver.a11y_node_named("Resize sidebar").unwrap();
        driver.perform_a11y_action(sidebar, &Action::Focus).unwrap();
        assert!(driver.root::<ChatApp>().keyboard_target.is_none());
        let inspector = driver.a11y_node_named("Resize inspector").unwrap();
        driver
            .perform_a11y_action(inspector, &Action::Focus)
            .unwrap();
        let root = driver.root::<ChatApp>();
        assert!(!root.left_flap.resize_accessibility().unwrap().state.focused);
        assert!(
            root.right_flap
                .resize_accessibility()
                .unwrap()
                .state
                .focused
        );

        let children = root.accessibility().unwrap().children;
        let left = children
            .iter()
            .position(|node| node.action_target.as_deref() == Some("sidebar-resize"))
            .unwrap();
        let right = children
            .iter()
            .position(|node| node.action_target.as_deref() == Some("inspector-resize"))
            .unwrap();
        let root = driver.root_mut::<ChatApp>();
        assert!(root.focus_synthetic_accessibility(&[left]));
        assert!(root.focus_synthetic_accessibility(&[right]));
        assert!(!root.left_flap.resize_accessibility().unwrap().state.focused);
        assert!(
            root.right_flap
                .resize_accessibility()
                .unwrap()
                .state
                .focused
        );
    }

    #[test]
    fn sidebar_resize_uses_the_live_host_pointer_and_semantic_routes() {
        use ui::Widget;
        use ui::headless::FrameHarness;
        let (app, doc) = app_with_doc();
        let mut state = ui::State::new();
        assert!(state.adopt_doc(doc.chat));
        let mut driver = FrameHarness::for_root(state, app, (960, 720), ui::Theme::dark());
        assert!(!driver.root::<ChatApp>().left_overlay());
        driver.resize((540, 720)).expect("normal narrow layout");
        assert!(driver.root::<ChatApp>().left_overlay());
        driver.resize((960, 720)).expect("normal wide layout");
        assert!(!driver.root::<ChatApp>().left_overlay());
        let before = driver.state_mut().doc.get_heads();
        let edge = driver.root::<ChatApp>().left_flap.sidebar_rect().x1;
        driver.drag_at((edge, 200.0), (edge - 40.0, 200.0));
        assert_eq!(
            driver.root::<ChatApp>().left_flap.list_rect().width(),
            240.0
        );
        assert!(!driver.root::<ChatApp>().wants_pointer_capture());
        let access = driver.root::<ChatApp>().accessibility().unwrap();
        for child in &access.children {
            if !matches!(
                child.action_target.as_deref(),
                Some("sidebar-resize" | "inspector-resize")
            ) {
                assert!(
                    child.semantic_key.is_none(),
                    "legacy Chat controls keep typed domain authority"
                );
            }
        }
        let resources = driver.resources();
        let resize = resources
            .iter()
            .find(|resource| {
                resource
                    .actions
                    .iter()
                    .any(|action| action.id == "set-value")
                    && resource.label == "Resize sidebar"
            })
            .expect("the live shared separator publishes exact resize alongside steps");
        for id in ["increment", "decrement", "set-value"] {
            assert!(resize.actions.iter().any(|action| action.id == id));
        }
        let address = ui::tilo_actions::door::survey(std::slice::from_ref(resize))
            .into_iter()
            .find(|offered| offered.address.action == "set-value")
            .expect("the separator offers set-value")
            .address;
        let arguments = serde_json::Map::from_iter([(
            ui::tilo_actions::PROMPTED_PARAM.to_owned(),
            serde_json::Value::String("30%".into()),
        )]);
        assert!(
            driver
                .invoke_action(&address, &arguments, false, None)
                .expect("the freshly published canonical resize door")
                .completed()
                .is_some()
        );
        let root = driver.root::<ChatApp>();
        assert_eq!(root.left_flap.sidebar_rect().width(), 288.0);
        assert_eq!(root.left_flap.list_rect().width(), 288.0);
        assert_eq!(
            driver.state_mut().doc.get_heads(),
            before,
            "resizing never changes the shared conversation"
        );
    }

    #[test]
    fn navigation_remains_docked_while_the_shared_conversation_detail_minimum_fits() {
        let (mut app, _doc) = app_with_doc();
        render_frame(&mut app, 760.0, 720.0);
        assert!(
            !app.left_overlay(),
            "sidebar + conversation detail fit at this width"
        );
        assert!(
            app.left_flap.content_rect().width()
                >= f64::from(ui::tokens::Metrics::BASE.detail_min_width)
        );
        let content = app.content_rect();
        for control in [Hit::FocusMain, Hit::Send, Hit::Attach] {
            let rect = app
                .hits
                .iter()
                .find_map(|(rect, hit)| {
                    (hit.semantic_id() == control.semantic_id()).then_some(*rect)
                })
                .expect("the actual conversation paints every composer control");
            assert!(
                content.contains_rect(rect),
                "composer controls fit beside navigation"
            );
        }
        render_frame(&mut app, 540.0, 720.0);
        assert!(
            app.left_overlay(),
            "navigation becomes a drawer when conversation detail no longer fits"
        );
    }

    #[test]
    fn the_attached_call_window_rests_below_the_header_row() {
        for (w, h) in [(1280.0, 800.0), (800.0, 600.0)] {
            let (mut app, _doc) = app_with_doc();
            app.thread_open = Some("m_concept".into());
            app.right_flap.open();
            app.call.join("c_huddle");
            render_frame(&mut app, w, h);
            render_frame(&mut app, w, h);
            let panel = app.huddle_panel();
            let header = app.rect.y0 + app.chrome.header_h;
            assert!(
                panel.y0 >= header,
                "{w}x{h}: the call window ({:.1}) covers the header row ({header:.1})",
                panel.y0
            );
            assert!(
                !app.hits.iter().any(|(rect, hit)| {
                    matches!(hit, Hit::StartCall | Hit::Search) && rect.overlaps(panel)
                }),
                "{w}x{h}: the call window covers a header control"
            );
        }
    }

    #[test]
    fn agent_work_thread_survives_pointer_navigation_and_projects_updates() {
        use crate::model::{AgentStepOutcome, AgentWork, AgentWorkState};
        use ui::Widget;

        for (width, height) in [(1600.0, 900.0), (800.0, 600.0)] {
            let (mut app, mut doc) = app_with_doc();
            stage_agent_work(&mut app, &mut doc);
            app.land(&mut doc);
            app.sync(&doc.chat, &automerge::ROOT);
            app.rect = vello::kurbo::Rect::new(0.0, 0.0, width, height);
            app.reconcile_flaps();
            app.right_flap.snap(true);
            render_frame(&mut app, width, height);

            let description = |app: &ChatApp| {
                app.accessibility()
                    .unwrap()
                    .children
                    .into_iter()
                    .find(|node| node.name.as_deref() == Some("Messages"))
                    .unwrap()
                    .children
                    .into_iter()
                    .find(|node| node.name.as_deref() == Some("Tide (agent): "))
                    .expect("the open thread exposes its agent reply")
                    .description
                    .unwrap()
            };
            assert!(description(&app).contains("Latest intent: Add function"));
            assert!(description(&app).contains("Requested: Add function"));

            let close = app
                .hits
                .iter()
                .find(|(_, hit)| matches!(hit, Hit::CloseThread))
                .expect("painted thread has a close/back control")
                .0
                .center();
            click_point(&mut app, &mut doc, close);
            assert!(app.thread_open.is_none());

            // Render the settled closed flap, not the outgoing overlay frame
            // that deliberately continues intercepting input during its exit.
            app.right_flap.snap(false);

            render_frame(&mut app, width, height);
            let open = app
                .hits
                .iter()
                .find(|(_, hit)| matches!(hit, Hit::OpenThread(id) if id == "m_agent_request"))
                .expect("the original request retains its reply affordance")
                .0
                .center();
            click_point(&mut app, &mut doc, open);
            assert_eq!(app.thread_open.as_deref(), Some("m_agent_request"));

            let mut applied_step = app
                .messages
                .iter()
                .find(|message| message.id == crate::testing::agent_reply())
                .unwrap()
                .work
                .as_ref()
                .unwrap()
                .trace[0]
                .clone();
            applied_step.outcome = AgentStepOutcome::Applied;
            crate::testing::apply_part_as(
                &mut doc,
                crate::resource::DEMO_AGENT,
                crate::testing::record_work(
                    crate::resource::DEMO_SUMMONS,
                    &AgentWork::new(
                        AgentWorkState::Done {
                            did: "Revenue curve added".into(),
                        },
                        crate::demo::NOW,
                    )
                    .with_trace(vec![applied_step]),
                ),
            )
            .unwrap();
            app.land(&mut doc);
            app.sync(&doc.chat, &automerge::ROOT);
            render_frame(&mut app, width, height);
            assert_eq!(
                description(&app),
                "Completed: Revenue curve added; Applied: Add function",
                "the state update retains the previously recorded dispatch evidence"
            );
            let reply = app
                .messages
                .iter()
                .find(|message| message.id == crate::testing::agent_reply())
                .unwrap();
            assert_eq!(reply.parent.as_deref(), Some("m_agent_request"));
            assert!(
                reply.text.is_empty(),
                "work updates do not overwrite authored text"
            );
        }
    }

    fn count_hits(app: &ChatApp, pred: impl Fn(&Hit) -> bool) -> usize {
        app.hits.iter().filter(|(_, h)| pred(h)).count()
    }

    fn click_point(app: &mut ChatApp, doc: &mut AutoCommit, point: Point) {
        feed(app, doc, &cursor_moved(point.x, point.y));
        feed(app, doc, &mouse(ElementState::Pressed, MouseButton::Left));
        feed(app, doc, &mouse(ElementState::Released, MouseButton::Left));
    }

    #[test]
    fn agent_work_compact_thread_scrolls_all_evidence_without_moving_timeline() {
        use crate::model::{AgentStep, AgentStepOutcome, AgentWork, AgentWorkState};
        use ui::Widget;
        let (mut app, mut doc) = app_with_doc();
        stage_agent_work(&mut app, &mut doc);
        app.sync(&doc.chat, &automerge::ROOT);
        let work = AgentWork::new(AgentWorkState::Done { did: "Reviewed all operations".into() }, crate::demo::NOW)
            .with_trace((0..60).map(|index| AgentStep::authored("calculator.workspace",
                format!("Operation {index}: preserve graph bounds and add the requested revenue curve"),
                AgentStepOutcome::Applied, index)).collect());
        crate::testing::apply_part_as(
            &mut doc,
            crate::resource::DEMO_AGENT,
            crate::testing::record_work(crate::resource::DEMO_SUMMONS, &work),
        )
        .unwrap();
        app.sync(&doc.chat, &automerge::ROOT);
        render_frame(&mut app, 800.0, 600.0);
        let region = app.primary_vertical_scroll().unwrap();
        assert_eq!(region.key, "thread");
        assert!(
            region.metrics.max_scroll() > 600.0,
            "production wrapping creates real overflow"
        );
        let timeline_offset = app.scroll;
        assert!(matches!(
            app.apply_primary_vertical_scroll(ui::widget::PrimaryScrollAction::End),
            ui::widget::PrimaryScrollResult::Changed(_)
        ));
        render_frame(&mut app, 800.0, 600.0);
        let region = app.primary_vertical_scroll().unwrap();
        assert!((region.metrics.offset - region.metrics.max_scroll()).abs() < 0.01);
        assert!((app.scroll - timeline_offset).abs() < 0.01);
        let tree = app.accessibility().unwrap();
        let messages = tree
            .children
            .iter()
            .find(|node| node.name.as_deref() == Some("Messages"))
            .unwrap();
        assert_eq!(
            messages.children.len(),
            2,
            "pushed detail exposes only parent and reply"
        );
        let description = messages
            .children
            .iter()
            .find_map(|node| node.description.as_ref())
            .unwrap();
        assert!(
            description.contains("Operation 59:"),
            "the final recorded dispatch is accessible"
        );
        let point = app.thread_body.center();
        feed(&mut app, &mut doc, &cursor_moved(point.x, point.y));
        feed(&mut app, &mut doc, &wheel(2.0));
        assert!(
            f64::from(app.thread_scroll.offset_y()) < region.metrics.max_scroll(),
            "pointer wheel reaches earlier evidence"
        );
        assert!((app.scroll - timeline_offset).abs() < 0.01);
    }

    use ui::synthetic_input::cursor_moved;
    fn mouse(state: ElementState, button: MouseButton) -> WindowEvent {
        ui::synthetic_input::mouse_input(state, button)
    }
    fn wheel(y: f64) -> WindowEvent {
        ui::synthetic_input::mouse_wheel(0.0, y as f32)
    }
    fn feed(app: &mut ChatApp, doc: &mut AutoCommit, ev: &WindowEvent) {
        let mods = Modifiers::default();
        let mut handled = false;
        app.dispatch_event(
            ev,
            1.0,
            mods,
            &mut ui::widget::Edits::lend(doc),
            &mut handled,
        );
    }

    use ui::synthetic_input::named_key;

    use ui::synthetic_input::char_key;

    /// Type each character of `s` into the app through the real keyboard path
    /// (space via the named key, like winit reports it).
    fn type_str(app: &mut ChatApp, doc: &mut AutoCommit, s: &str) {
        for ch in s.chars() {
            let ev = if ch == ' ' {
                named_key(NamedKey::Space)
            } else {
                char_key(&ch.to_string())
            };
            feed(app, doc, &ev);
        }
    }

    fn mods(ctrl: bool, shift: bool) -> Modifiers {
        use winit::keyboard::ModifiersState;
        let mut s = ModifiersState::empty();
        if ctrl {
            s |= ModifiersState::CONTROL;
        }
        if shift {
            s |= ModifiersState::SHIFT;
        }
        Modifiers::from(s)
    }

    fn feed_mods(app: &mut ChatApp, doc: &mut AutoCommit, ev: &WindowEvent, m: Modifiers) {
        let mut handled = false;
        app.dispatch_event(ev, 1.0, m, &mut ui::widget::Edits::lend(doc), &mut handled);
    }

    // --- contextual channels + cross-workspace Inbox -------------------

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one labelled phase per original scenario, each with its own fresh fixture"
    )]
    fn visible_controls_publish_unique_accessible_semantics() {
        // Phase: accessibility_names_contextual_navigation_and_conversation_content.
        {
            let (mut app, _doc) = app_with_doc();
            render_frame(&mut app, 1280.0, 720.0);
            let root = ui::Widget::accessibility(&app).expect("Chat has a semantic root");
            assert_eq!(root.role, ui::a11y::Role::Group);
            assert_eq!(root.name.as_deref(), Some("Chat"));
            assert!(root.children.iter().any(|node| {
                node.name
                    .as_deref()
                    .is_some_and(|name| name.starts_with("Inbox"))
            }));
            assert!(
                root.children
                    .iter()
                    .all(|node| node.name.as_deref() != Some("New Workspace")),
                "Chat never exposes a second workspace creation concept"
            );
            let messages = root
                .children
                .iter()
                .find(|node| node.name.as_deref() == Some("Messages"))
                .expect("conversation content is exposed as a list");
            assert!(!messages.children.is_empty());
            let channel = app.channel(&app.active_channel).expect("active channel");
            let heading = root
                .children
                .iter()
                .find(|node| node.role == ui::a11y::Role::Heading { level: 1 })
                .expect("the conversation header remains semantic when paint elides it");
            assert!(
                heading
                    .name
                    .as_deref()
                    .is_some_and(|name| name.contains(&channel.name))
            );
            assert_eq!(heading.description.as_deref(), Some(channel.topic.as_str()));
        }

        // Phase: narrow_elided_header_and_icon_controls_publish_hover_help.
        {
            let (mut app, _doc) = app_with_doc();
            app.stage_view("narrow");
            render_frame(&mut app, 390.0, 844.0);
            let channel = app.channel(&app.active_channel).expect("active channel");
            // Somewhere over the elided title, between the header's buttons.
            let header = app.header_rect();
            let header_tip = (0..40)
                .map(|step| header.x0 + header.width() * f64::from(step) / 40.0)
                .filter_map(|x| app.hover_tooltip_target(Point::new(x, header.center().y)))
                .find(|(rect, _)| *rect == header)
                .map(|(_, label)| label)
                .expect("the elided header has a full hover label");
            assert!(header_tip.contains(&channel.name));
            assert!(header_tip.contains(&channel.topic));

            let (rect, _) = app
                .hits
                .iter()
                .find(|(_, hit)| matches!(hit, Hit::ToggleLeftFlap))
                .expect("narrow navigation button");
            assert_eq!(
                app.hover_tooltip_target(rect.center())
                    .map(|(_, label)| label),
                Some("Show conversation navigation".into())
            );
        }

        // Phase: semantic_text_inputs_accept_focus_value_and_selection_without_sending.
        {
            let (mut app, mut doc) = app_with_doc();
            render_frame(&mut app, 1280.0, 720.0);
            let target = app
                .semantic_hits()
                .into_iter()
                .find_map(|(_, hit, ..)| matches!(hit, Hit::FocusMain).then(|| hit.semantic_id()))
                .expect("the painted main composer has a semantic target");
            let root = ui::Widget::accessibility(&app).expect("Chat has a semantic root");
            let composer = root
                .children
                .iter()
                .find(|node| node.action_target.as_deref() == Some(target.as_str()))
                .expect("the composer is retained");
            assert_eq!(composer.role, ui::a11y::Role::MultilineTextInput);
            assert_eq!(composer.value.as_deref(), Some(""));

            assert!(ui::Widget::perform_semantic_action(
                &mut app,
                &target,
                &ui::a11y::Action::Focus,
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ));
            assert!(ui::Widget::perform_semantic_action(
                &mut app,
                &target,
                &ui::a11y::Action::SetValue("🦀 Accessible draft".into()),
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ));
            assert_eq!(
                app.composer_main.value(),
                "🦀 Accessible draft",
                "semantic editing updates the person's unsent draft"
            );
            assert!(ui::Widget::perform_semantic_action(
                &mut app,
                &target,
                &ui::a11y::Action::SetSelection {
                    anchor: 0,
                    focus: 1,
                },
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ));
            let (anchor, caret) = app.composer_main.selection();
            assert_eq!(
                (anchor.min(caret), anchor.max(caret)),
                (0, "🦀".len()),
                "grapheme-indexed semantic endpoints become UTF-8 offsets only inside the editor"
            );
            assert!(
                app.last_dispatched().is_none(),
                "draft remains unsent until Send"
            );
        }

        // Phase: every_visible_scenario_has_unique_semantic_targets.
        {
            for (scenario, width, height) in [
                ("", 1280.0, 720.0),
                ("narrow", 390.0, 844.0),
                ("thread-narrow", 390.0, 844.0),
                ("context", 1280.0, 720.0),
                ("attach", 1280.0, 720.0),
                ("attach-loading", 1280.0, 720.0),
                ("attach-error", 1280.0, 720.0),
                ("edit", 1280.0, 720.0),
                ("composer", 1280.0, 720.0),
                ("collaboration", 1280.0, 720.0),
            ] {
                let (mut app, _doc) = app_with_doc();
                app.stage_view(scenario);
                render_frame(&mut app, width, height);
                let targets: Vec<_> = app
                    .semantic_hits()
                    .into_iter()
                    .map(|(_, hit, ..)| hit.semantic_id())
                    .collect();
                let unique: std::collections::HashSet<_> = targets.iter().collect();
                assert_eq!(
                    unique.len(),
                    targets.len(),
                    "scenario {scenario:?} publishes stable unique semantic targets"
                );
            }
        }
    }

    #[test]
    fn a_mention_completed_with_tab_is_sent_as_a_marked_reference() {
        let (mut app, mut doc) = app_with_doc();
        app.focus = Focus::Main;
        app.set_composer_focus();
        // The editor learns who the room offers at `@` as it is drawn.
        render_frame(&mut app, 1280.0, 720.0);
        let who = app
            .participants()
            .into_iter()
            .find(|participant| !app.is_me(&participant.id) && participant.name.len() > 2)
            .expect("the demo room has someone to mention");
        let prefix: String = who.name.chars().take(2).collect();
        type_str(&mut app, &mut doc, &format!("ping @{prefix}"));
        feed(&mut app, &mut doc, &named_key(NamedKey::Tab));
        let label = format!("@{}", who.name);
        assert_eq!(app.composer_main.value(), format!("ping {label} "));
        type_str(&mut app, &mut doc, "now");
        feed(&mut app, &mut doc, &named_key(NamedKey::Enter));
        let sent = crate::testing::messages(&doc, &app.active_channel)
            .into_iter()
            .last()
            .unwrap();
        assert_eq!(sent.text, format!("ping {label} now"));
        assert_eq!(sent.mentions.len(), 1, "{:?}", sent.mentions);
        assert_eq!(sent.mentions[0].user, who.id);
        assert_eq!(sent.mentions[0].slice(&sent.text), label);
        assert!(
            sent.blocks()[0]
                .marks
                .iter()
                .any(|span| span.mark == ui::widget::Mark::Mention(who.id.clone())),
            "the body keeps the mention as written"
        );
        // Rendered through the read-only view, it lays out at its width.
        render_frame(&mut app, 1280.0, 720.0);
        assert!(!app.body_views.borrow().is_empty());
    }

    #[test]
    fn editing_a_formatted_message_keeps_its_formatting() {
        let (mut app, mut doc) = app_with_doc();
        app.focus = Focus::Main;
        app.set_composer_focus();
        type_str(&mut app, &mut doc, "plain ");
        feed_mods(&mut app, &mut doc, &char_key("b"), mods(true, false));
        type_str(&mut app, &mut doc, "bold");
        feed(&mut app, &mut doc, &named_key(NamedKey::Enter));
        let sent = crate::testing::messages(&doc, &app.active_channel)
            .into_iter()
            .last()
            .unwrap();
        assert_eq!(sent.text, "plain bold");
        assert!(
            sent.blocks()[0]
                .marks
                .iter()
                .any(|span| span.mark == ui::widget::Mark::Bold && span.range == (6..10))
        );
        app.reload(&doc);
        app.begin_edit(sent.id.clone());
        assert_eq!(app.edit_input.value(), "plain bold");
        type_str(&mut app, &mut doc, "!");
        feed(&mut app, &mut doc, &named_key(NamedKey::Enter));
        let edited = app.message(&sent.id).unwrap();
        assert_eq!(edited.text, "plain bold!");
        assert!(
            edited.blocks()[0]
                .marks
                .iter()
                .any(|span| span.mark == ui::widget::Mark::Bold),
            "the edit keeps the bold run"
        );
        assert_eq!(edited.history.last().unwrap().text, "plain bold");
    }

    #[test]
    fn composer_accessibility_keeps_a_backward_browser_selection_direction() {
        let (mut app, mut doc) = app_with_doc();
        crate::testing::write_draft(&mut app, &mut doc, "Ae\u{301}👋B");
        app.sync_composers(&doc);
        render_frame(&mut app, 1280.0, 720.0);
        assert!(app.composer_main.perform_action(
            &ui::a11y::Action::SetSelection {
                anchor: 4,
                focus: 2,
            },
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));

        let root = ui::Widget::accessibility(&app).expect("Chat has a semantic root");
        let composer = root
            .children
            .iter()
            .find(|node| node.action_target.as_deref() == Some("chat.composer"))
            .expect("the main composer remains semantic");
        assert_eq!(
            composer.text_selection,
            Some(ui::a11y::AccessibleTextSelection {
                anchor: 9,
                focus: 4
            }),
            "the semantic bridge must retain the anchor at the right edge while Shift+Left moves focus backward"
        );
    }

    #[test]
    fn reactions_toggle_through_the_picker_and_chips_on_the_shared_route() {
        // Phase: reaction_via_the_emoji_picker_round_trips_through_the_ui.
        {
            let (mut app, mut doc) = app_with_doc();
            let reacted = |app: &ChatApp| {
                app.message("m_me1")
                    .unwrap()
                    .reactions
                    .iter()
                    .any(|r| r.emoji == "🔥" && r.reacted_by(model::ME))
            };
            assert!(!reacted(&app), "m_me1 starts without my 🔥");
            // Open the add-reaction picker on the message, then pick an emoji.
            app.on_hit(
                Hit::AddReaction {
                    msg: "m_me1".into(),
                    anchor: Point::ZERO,
                },
                &mut ui::widget::Edits::lend(&mut doc),
            );
            assert!(app.emoji_open && app.emoji_target.as_deref() == Some("m_me1"));
            app.on_hit(
                Hit::EmojiChoice("🔥".into()),
                &mut ui::widget::Edits::lend(&mut doc),
            );
            assert!(!app.emoji_open, "picking closes the picker");
            assert!(reacted(&app), "the picked emoji became my reaction");
            // Re-opening and picking the same emoji toggles it back off.
            app.on_hit(
                Hit::AddReaction {
                    msg: "m_me1".into(),
                    anchor: Point::ZERO,
                },
                &mut ui::widget::Edits::lend(&mut doc),
            );
            app.on_hit(
                Hit::EmojiChoice("🔥".into()),
                &mut ui::widget::Edits::lend(&mut doc),
            );
            assert!(!reacted(&app), "picking it again removed the reaction");
        }

        // Phase: reaction_chips_have_unique_toggle_semantics_and_use_the_shared_action_route.
        {
            let (mut app, mut doc) = app_with_doc();
            render_frame(&mut app, 1280.0, 720.0);

            let (_, reaction, _, role, checked) = app
                .semantic_hits()
                .into_iter()
                .find(|(_, hit, ..)| matches!(hit, Hit::Reaction { .. }))
                .expect("a painted reaction chip is a semantic control");
            assert_eq!(role, ui::a11y::Role::ToggleButton);
            let target = reaction.semantic_id();
            let root = ui::Widget::accessibility(&app).expect("Chat has a semantic root");
            let targets: std::collections::HashSet<_> = root
                .children
                .iter()
                .filter_map(|node| node.action_target.as_deref())
                .collect();
            assert_eq!(
                targets.len(),
                root.children
                    .iter()
                    .filter(|node| node.action_target.is_some())
                    .count(),
                "visible controls do not publish duplicate semantic targets"
            );
            let reaction_node = root
                .children
                .iter()
                .find(|node| node.action_target.as_deref() == Some(target.as_str()))
                .expect("the reaction target is retained in the accessibility tree");
            assert_eq!(reaction_node.role, ui::a11y::Role::ToggleButton);
            assert_eq!(reaction_node.state.checked, Some(checked));

            assert!(ui::Widget::perform_semantic_action(
                &mut app,
                &target,
                &ui::a11y::Action::Activate,
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ));
            let (_, _, _, _, after) = app
                .semantic_hits()
                .into_iter()
                .find(|(_, hit, ..)| hit.semantic_id() == target)
                .expect("the reaction remains visible after toggling");
            assert_ne!(after, checked, "semantic activation toggles my reaction");
            let action = serde_json::to_value(
                app.dispatched_parts
                    .borrow()
                    .last()
                    .cloned()
                    .expect("the semantic control dispatches through the door"),
            )
            .unwrap();
            assert!(
                action["action"] == "react" || action["action"] == "unreact",
                "{action}"
            );
        }
    }

    // --- members roster (right flap "members" half was unimplemented) ---

    #[test]
    fn roster_projects_people_without_guessing_stream_identity_or_legacy_presence() {
        let (mut app, _) = app_with_doc();
        let people = app.users.len();
        service_streams(&mut app, "c_huddle", 3);
        let rows = app.roster_rows();
        assert_eq!(rows.len(), people);
        assert!(
            rows.iter()
                .filter(|(person, _)| !app.is_me(&person.id))
                .all(|(_, channel)| channel.is_none()),
            "an anonymous stream places nobody in the call"
        );
        for stream in app.voice_in("c_huddle") {
            assert!(app.user_name(&stream.user).starts_with("Audio stream "));
        }
    }

    #[test]
    fn roster_shows_only_this_workspaces_people_and_collaboration_controls() {
        // Phase: workspace_people_do_not_leak_from_the_global_identity_directory.
        {
            let (mut app, mut doc) = app_with_doc();
            assert!(
                app.users.iter().any(|user| user.id == "u_aria"),
                "demo precondition: the first workspace has other known people"
            );

            // A fresh workspace owns empty text/voice channels but shares the
            // document's identity directory. Its People/call chrome must stay
            // quiet until someone actually participates in one of its channels.
            crate::testing::apply_as(
                &mut doc,
                model::ME,
                crate::resource::Chat::CREATE_WORKSPACE.with(chat::CreateWorkspace {
                    name: "Private draft".into(),
                }),
            )
            .unwrap();
            let fresh = crate::projection::read(&doc)
                .workspace_rows()
                .into_iter()
                .find(|workspace| workspace.name == "Private draft")
                .expect("the workspace is made")
                .id;
            app.active_ws = fresh;
            app.active_channel.clear();
            app.thread_open = None;
            app.reload(&doc);

            assert!(
                app.users.iter().all(|user| app.is_me(&user.id)),
                "only the local identity is evidenced in the fresh workspace; got {:?}",
                app.users.iter().map(|user| &user.id).collect::<Vec<_>>()
            );
            assert!(!app.has_other_collaborator());
            render_frame(&mut app, 1280.0, 720.0);
            assert_eq!(
                count_hits(&app, |hit| matches!(
                    hit,
                    Hit::StartCall | Hit::ToggleRightFlap
                )),
                0,
                "global directory entries must not surface People/call controls in a private workspace"
            );
        }

        // Phase: private_workspace_keeps_collaboration_controls_out_of_the_header.
        {
            let mut doc = AutoCommit::new();
            crate::testing::starter(&mut doc, model::ME, "You", "QD");
            let mut app = ChatApp::new_ephemeral();
            app.active_ws = "ws_home".into();
            app.active_channel = "c_general".into();
            app.thread_open = None;
            app.reload(&doc);
            render_frame(&mut app, 1280.0, 720.0);

            assert_eq!(
                count_hits(&app, |h| matches!(h, Hit::StartCall | Hit::ToggleRightFlap)),
                0,
                "a private workspace has no People or call controls in its resting header"
            );
        }
    }

    // --- unread badges (were seed-only fake data) -----------------------

    /// Lay the app out once in a real viewport, as every presenting runner
    /// (native window, browser canvas, offscreen shot) does before drawing.
    fn present(app: &mut ChatApp) {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: std::collections::HashMap<NodeId, *mut dyn Widget> =
            std::collections::HashMap::new();
        let theme = ui::Theme::default();
        let node = app.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(1200.0),
                    height: taffy::AvailableSpace::Definite(800.0),
                },
            )
            .unwrap();
        app.apply_layout(&taffy, node, vello::kurbo::Point::ORIGIN);
    }

    #[test]
    fn headless_host_worker_sync_never_authors_read_receipts() {
        // A hosted/headless worker only pumps the model (update + sync) and
        // never lays out: read state belongs to the person's device, so the
        // host identity must not append "Mark channel read" changes.
        let (mut app, mut doc) = app_with_doc();
        let channel = app.active_channel.clone();
        crate::testing::apply_part_at(
            &mut doc,
            "u_aria",
            (crate::demo::NOW + 1) * 1000,
            crate::testing::post(&channel, "arrived while nobody was looking", None),
        )
        .unwrap();
        let watermark = crate::testing::read_mark(&doc, &channel, &app.me.id);
        let heads = doc.get_heads();
        for _ in 0..5 {
            assert!(!app.update(&doc.chat, &automerge::ROOT));
            app.sync(&doc.chat, &automerge::ROOT);
        }
        assert_eq!(
            crate::testing::read_mark(&doc, &channel, &app.me.id),
            watermark,
            "an unpresented worker must not advance the read watermark"
        );
        assert_eq!(
            doc.get_heads(),
            heads,
            "an unpresented worker authored a change"
        );

        // The same instance once presented to a person marks the channel in
        // view read on the next model frame, without waiting for input.
        present(&mut app);
        assert!(
            app.needs_animation_frame(),
            "first presentation requests a model frame"
        );
        assert!(
            app.update(&doc.chat, &automerge::ROOT),
            "first presentation re-runs sync"
        );
        app.sync(&doc.chat, &automerge::ROOT);
        app.land(&mut doc);
        assert!(
            crate::testing::read_mark(&doc, &channel, &app.me.id) > watermark,
            "the presented channel is marked read"
        );
        assert!(
            !app.update(&doc.chat, &automerge::ROOT),
            "the extra frame is one-shot"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one labelled phase per original scenario, each with its own fresh fixture"
    )]
    fn reading_position_drives_unread_state_and_the_return_to_latest_control() {
        // Phase: opening_a_channel_marks_it_read_and_clears_unread.
        {
            let (mut app, mut doc) = app_with_doc();
            // Simulate having viewed the initial channel (as each sync frame does).
            app.mark_active_read(&doc);
            app.land(&mut doc);
            app.reload(&doc);
            let unread =
                |app: &ChatApp, id: &str| app.channels.iter().find(|c| c.id == id).unwrap().unread;
            assert_eq!(
                unread(&app, "c_general"),
                1,
                "an unseen welcome message shows as unread"
            );
            app.on_hit(
                Hit::Channel("c_general".into()),
                &mut ui::widget::Edits::lend(&mut doc),
            );
            app.land(&mut doc);
            app.reload(&doc);
            assert_eq!(app.active_channel, "c_general");
            assert_eq!(
                unread(&app, "c_general"),
                0,
                "opening the channel clears its unread"
            );
            // Leaving it keeps it read — the watermark persisted through the mutation.
            app.on_hit(
                Hit::Channel("c_crit".into()),
                &mut ui::widget::Edits::lend(&mut doc),
            );
            app.land(&mut doc);
            app.reload(&doc);
            assert_eq!(
                unread(&app, "c_general"),
                0,
                "a read channel stays read after leaving"
            );
        }

        // Phase: a_message_in_another_channel_shows_as_unread.
        {
            let (mut app, mut doc) = app_with_doc();
            app.mark_active_read(&doc);
            app.land(&mut doc);
            app.reload(&doc);
            let unread =
                |app: &ChatApp, id: &str| app.channels.iter().find(|c| c.id == id).unwrap().unread;
            assert_eq!(
                unread(&app, "c_random"),
                0,
                "an empty channel has nothing unread"
            );
            // A peer posts to a channel we're not viewing (arrives via the CRDT).
            crate::testing::apply_part_at(
                &mut doc,
                "u_aria",
                crate::demo::NOW * 1000,
                crate::testing::post("c_random", "anyone around?", None),
            )
            .unwrap();
            app.reload(&doc);
            assert_eq!(
                unread(&app, "c_random"),
                1,
                "the unseen message is counted unread"
            );
        }

        // Phase: mouse_wheel_scrolls_the_timeline.
        {
            let (mut app, mut doc) = app_with_doc();
            assert_eq!(app.scroll, 0.0);
            feed(&mut app, &mut doc, &wheel(3.0));
            assert!(app.scroll > 0.0, "scrolling moves the timeline");
            let up = app.scroll;
            feed(&mut app, &mut doc, &wheel(-5.0));
            assert!(app.scroll < up, "scrolling the other way moves it back");
        }

        // Phase: reading_history_does_not_mark_new_messages_read_until_returning_to_latest.
        {
            let (mut app, mut doc) = app_with_doc();
            app.mark_active_read(&doc);
            app.land(&mut doc);
            let watermark = crate::testing::read_mark(&doc, &app.active_channel, &app.me.id);

            // A remote message arrives after the current read boundary. Being in
            // the room is not enough to acknowledge it once the reader has moved
            // back through history.
            crate::testing::apply_part_at(
                &mut doc,
                "u_aria",
                (crate::demo::NOW + 1) * 1000,
                crate::testing::post(
                    &app.active_channel,
                    "A newer update while you are reading history",
                    None,
                ),
            )
            .unwrap();
            app.timeline_at_end = false;
            app.mark_active_read(&doc);
            app.land(&mut doc);
            assert_eq!(
                crate::testing::read_mark(&doc, &app.active_channel, &app.me.id),
                watermark,
                "a reader away from the live edge does not send a false read receipt"
            );

            app.jump_to_latest();
            app.mark_active_read(&doc);
            app.land(&mut doc);
            assert!(
                crate::testing::read_mark(&doc, &app.active_channel, &app.me.id) > watermark,
                "the same message is acknowledged once the reader returns to it"
            );
        }

        // Phase: history_scroll_exposes_an_accessible_return_to_latest_control.
        {
            let (mut app, mut doc) = app_with_doc();
            // The ephemeral fixture opens a thread by default. On a compact
            // viewport that is now an honest pushed detail view, so navigate
            // back before testing the main timeline's history controls.
            app.on_hit(Hit::CloseThread, &mut ui::widget::Edits::lend(&mut doc));
            assert!(app.thread_open.is_none());
            for n in 0..24 {
                crate::testing::apply_part_at(
            &mut doc,
            "u_aria",
            (crate::demo::NOW + i64::from(n)) * 1000,
            crate::testing::post(&app.active_channel, &format!(
                            "Longer history line {n}: enough text to wrap inside a narrow conversation column."
                        ), None),
        )
        .unwrap();
            }
            app.reload(&doc);
            render_frame(&mut app, 390.0, 620.0);
            feed(&mut app, &mut doc, &wheel(12.0));
            render_frame(&mut app, 390.0, 620.0);

            assert!(
                !app.timeline_at_end,
                "history-directed wheel input leaves the live edge"
            );
            assert!(
                app.semantic_hits().iter().any(|(_, hit, name, role, _)| {
                    matches!(hit, Hit::JumpToLatest)
                        && name == "Jump to latest message"
                        && *role == ui::a11y::Role::Button
                }),
                "the return route is a labelled semantic button, not pointer-only chrome"
            );

            app.on_hit(Hit::JumpToLatest, &mut ui::widget::Edits::lend(&mut doc));
            assert_eq!(app.scroll, 0.0);
            assert!(app.timeline_at_end);
        }

        // Phase: ctrl_end_returns_to_the_live_edge_without_stealing_plain_end.
        {
            let (mut app, mut doc) = app_with_doc();
            app.scroll = -180.0;
            app.timeline_at_end = false;
            feed_mods(
                &mut app,
                &mut doc,
                &named_key(NamedKey::End),
                mods(true, false),
            );
            assert_eq!(app.scroll, 0.0);
            assert!(app.timeline_at_end);
        }
    }

    // --- sidebar mute/deafen (were dead outside a call) -----------------

    #[test]
    fn calls_start_join_leave_and_mute_through_shared_voice_membership() {
        // Phase: start_call_creates_a_voice_channel_when_the_workspace_has_none.
        {
            let (mut app, mut doc) = app_with_doc();
            app.on_hit_workspace("ws_jam".into());
            app.reload(&doc);
            assert_eq!(app.active_ws, "ws_jam");
            assert!(
                !app.channels
                    .iter()
                    .any(|c| c.workspace == "ws_jam" && c.kind == ChannelKind::Voice),
                "the weekend-jam workspace starts text-only"
            );
            app.on_hit(Hit::StartCall, &mut ui::widget::Edits::lend(&mut doc));
            assert!(
                app.call.active,
                "the Call button starts a call even with no voice channel"
            );
            assert!(
                app.channels
                    .iter()
                    .any(|c| c.workspace == "ws_jam" && c.kind == ChannelKind::Voice),
                "a voice channel was created to hold the call"
            );
            assert!(app.call.muted);
            assert_eq!(
                app.native_voice.snapshot.state,
                ui::native_media::State::Preparing
            );
        }

        // Phase: header_call_button_joins_an_existing_voice_channel.
        {
            let (mut app, mut doc) = app_with_doc();
            assert!(!app.call.active);
            let voice_before = app
                .channels
                .iter()
                .filter(|c| c.workspace == "ws_acme" && c.kind == ChannelKind::Voice)
                .count();
            app.on_hit(Hit::StartCall, &mut ui::widget::Edits::lend(&mut doc));
            assert!(app.call.active);
            assert_eq!(
                app.channel(&app.call.channel).unwrap().kind,
                ChannelKind::Voice,
                "joined an existing voice channel"
            );
            let voice_after = app
                .channels
                .iter()
                .filter(|c| c.workspace == "ws_acme" && c.kind == ChannelKind::Voice)
                .count();
            assert_eq!(
                voice_before, voice_after,
                "no spurious voice channel created when one exists"
            );
        }

        // Phase: no_mute_or_deafen_controls_outside_a_call.
        {
            let (mut app, mut doc) = app_with_doc();
            assert!(!app.call.active);
            render_frame(&mut app, 1280.0, 720.0);
            assert_eq!(
                count_hits(&app, |h| matches!(h, Hit::CallMute)),
                0,
                "no mute control outside a call"
            );
            // Join a call → the sidebar identity bar now offers real toggles.
            app.on_hit(
                Hit::Channel("c_standup".into()),
                &mut ui::widget::Edits::lend(&mut doc),
            );
            app.land(&mut doc);
            app.reload(&doc);
            assert!(app.call.active);
            render_frame(&mut app, 1280.0, 720.0);
            let side = app.sidebar_rect();
            assert!(
                app.hits
                    .iter()
                    .any(|(r, h)| matches!(h, Hit::CallMute) && r.x1 <= side.x1 + 1.0),
                "in a call the sidebar shows a mute toggle"
            );
        }

        // Phase: follow_host_control_no_longer_exists.
        {
            let (app, _doc) = app_with_doc();
            let (_, buttons) = app.overlay_controls_layout();
            assert!(
                !buttons.iter().any(|(label, ..)| *label == "Follow host"),
                "the non-functional Follow-host toggle was removed"
            );
            // The genuinely-wired controls remain.
            assert!(buttons.iter().any(|(l, ..)| *l == "Unmute"));
            assert!(buttons.iter().any(|(l, ..)| *l == "Leave"));
        }
    }

    fn voice_channels(app: &ChatApp, ws: &str) -> usize {
        app.channels
            .iter()
            .filter(|c| c.workspace == ws && c.kind == ChannelKind::Voice)
            .count()
    }

    /// The joins sent to the native voice service (status polls aside).
    fn voice_joins(app: &mut ChatApp) -> Vec<ui::native_media::Command> {
        use ui::widget::Collaborative as _;
        app.take_collab_requests()
            .into_iter()
            .filter_map(|request| match request {
                ui::collaboration::CollabRequest::NativeMediaCommand { command, .. }
                    if matches!(command.command, ui::native_media::Command::Join { .. }) =>
                {
                    Some(command.command)
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn start_call_joins_the_workspace_voice_channel_creating_one_when_text_only() {
        use crate::effects::ChatEffect;
        // A text-only workspace gains a voice channel, and the call starts in
        // it once the channel is in the document.
        let (mut app, mut doc) = app_with_doc();
        app.on_hit_workspace("ws_jam".into());
        app.reload(&doc);
        assert_eq!(voice_channels(&app, "ws_jam"), 0);
        app.on_hit(Hit::StartCall, &mut ui::widget::Edits::lend(&mut doc));
        assert_eq!(voice_channels(&app, "ws_jam"), 1);
        assert!(app.call.active);
        let channel = app.channel(&app.call.channel).expect("call channel");
        assert_eq!(
            (channel.kind, channel.name.as_str()),
            (ChannelKind::Voice, "Voice")
        );
        assert_eq!(
            app.native_voice.snapshot.state,
            ui::native_media::State::Preparing
        );

        // A workspace with a voice channel joins it without making another,
        // through the published effect.
        let (mut app, _doc) = app_with_doc();
        let before = voice_channels(&app, "ws_acme");
        assert!(before > 0);
        app.answer_effect(ChatEffect::StartCall)
            .expect("start-call runs");
        assert!(app.call.active);
        assert_eq!(voice_channels(&app, "ws_acme"), before);
        assert_eq!(
            voice_joins(&mut app),
            vec![ui::native_media::Command::Join {
                channel_id: app.call.channel.clone()
            }]
        );

        // Invoking it again reveals the running call; it never closes it.
        app.call.expanded = false;
        for _ in 0..2 {
            app.answer_effect(ChatEffect::StartCall)
                .expect("start-call runs");
            assert!(app.call.active && app.call.expanded);
        }
        assert!(voice_joins(&mut app).is_empty(), "no second join");

        // The header button keeps its own collapse toggle.
        let (mut app, mut doc) = app_with_doc();
        app.on_hit(Hit::StartCall, &mut ui::widget::Edits::lend(&mut doc));
        app.call.expanded = true;
        app.on_hit(Hit::StartCall, &mut ui::widget::Edits::lend(&mut doc));
        assert!(app.call.active && !app.call.expanded);
        app.on_hit(Hit::StartCall, &mut ui::widget::Edits::lend(&mut doc));
        assert!(app.call.active && app.call.expanded);
    }

    #[test]
    fn start_video_call_is_refused_because_calls_are_voice_only() {
        let (mut app, mut doc) = app_with_doc();
        let before = doc.save();
        let error = app
            .answer_effect(crate::effects::ChatEffect::StartVideoCall)
            .expect_err("video calls are refused");
        assert_eq!(error, crate::effects::video_call_unavailable_reason());
        assert!(!app.call.active);
        assert!(voice_joins(&mut app).is_empty());
        assert_eq!(doc.save(), before);
    }

    /// Answer every voice request with the broker's reply when this app has
    /// no native media lane; returns how many there were.
    fn refuse_voice(app: &mut ChatApp) -> usize {
        use ui::widget::Collaborative as _;
        let mut sent = 0;
        for request in app.take_collab_requests() {
            if let ui::collaboration::CollabRequest::NativeMediaCommand { request_id, .. } = request
            {
                sent += 1;
                app.handle_collab_event(
                    &ui::collaboration::CollabEvent::NativeMediaCommandResult {
                        request_id,
                        result: Err("native media requires the bounded control lane".into()),
                    },
                );
            }
        }
        sent
    }

    #[test]
    fn a_refused_voice_service_is_not_asked_again_on_every_frame() {
        // Phase: not_in_a_call.
        {
            let (mut app, _doc) = app_with_doc();
            let sent: usize = (0..20).map(|_| refuse_voice(&mut app)).sum();
            assert_eq!(
                sent, 1,
                "one status poll per interval, not one request per frame"
            );
        }
        // Phase: joined_call_is_left_once.
        {
            let (mut app, mut doc) = app_with_doc();
            app.on_hit(
                Hit::Channel("c_standup".into()),
                &mut ui::widget::Edits::lend(&mut doc),
            );
            app.land(&mut doc);
            app.reload(&doc);
            assert!(
                app.native_voice.channel.is_some(),
                "opening a voice channel joins it"
            );
            let sent: usize = (0..20).map(|_| refuse_voice(&mut app)).sum();
            assert_eq!(sent, 2, "a refused join is left once, then nothing");
            assert!(app.native_voice.channel.is_none());
        }
    }

    #[test]
    fn microphone_intent_waits_for_local_service_and_never_writes_membership() {
        let (mut app, mut doc) = app_with_doc();
        app.on_hit(
            Hit::Channel("c_standup".into()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        app.land(&mut doc);
        app.reload(&doc);
        service_streams(&mut app, "c_standup", 0);
        let before = doc.save();
        assert!(app.call.muted);
        app.on_hit(Hit::CallMute, &mut ui::widget::Edits::lend(&mut doc));
        assert!(
            app.call.muted,
            "a microphone request is not local consent or active capture"
        );
        let requests = app.native_voice.take_requests(ui::time::now());
        assert!(requests.iter().any(|request| matches!(
            request,
            ui::collaboration::CollabRequest::NativeMediaCommand {
                command: ui::native_media::Request {
                    command: ui::native_media::Command::SetMuted { muted: false },
                    ..
                },
                ..
            }
        )));
        assert_eq!(doc.save(), before);
    }

    // --- call controls under reduced permissions ------------------------

    #[test]
    fn leave_remains_available_after_downgrade() {
        use ui::permissions::{PermissionContext, Role};
        let (mut app, mut doc) = app_with_doc();
        app.on_hit(
            Hit::Channel("c_huddle".into()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        app.land(&mut doc);
        app.reload(&doc);
        app.on_hit(Hit::CallExpand, &mut ui::widget::Edits::lend(&mut doc));
        app.perms = crate::permissions::ChatPermissions::from_context(PermissionContext::member(
            model::ME,
            Role::Viewer,
        ));
        app.on_hit(Hit::CallLeave, &mut ui::widget::Edits::lend(&mut doc));
        assert!(!app.call.active);
    }

    // --- header Call button (was unreachable without a voice channel) ---

    // --- Follow-host control removal ------------------------------------

    // --- reaction round-trip through the picker (test gap) --------------

    #[test]
    fn emoji_search_enter_picks_the_best_match() {
        let (mut app, mut doc) = app_with_doc();
        app.focus = Focus::Main;
        app.on_hit(
            Hit::ComposerEmoji(Point::ZERO),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        app.emoji_search.set_query("pizza");
        app.pick_first_emoji_result(&mut ui::widget::Edits::lend(&mut doc));
        assert_eq!(app.draft_main, "🍕");
        assert!(!app.emoji_open);
        assert_eq!(app.emoji_recents.items()[0], "🍕");
        // Reopening starts from an empty query.
        app.on_hit(
            Hit::ComposerEmoji(Point::ZERO),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        assert!(app.emoji_search.value().is_empty());
    }

    // --- swipe-to-delete gesture through the raw input path (test gap) --

    // --- category + reorder + thread + workspace via the hit path -------

    // --- multi-participant collaboration (the real sync path) ------------
    //
    // These drive two independent `ChatApp` instances against two Automerge
    // documents and shuttle the exact incremental deltas the compositor's
    // collab broker ships (`save_incremental` → peer `load_incremental` — see
    // `ui::App::broadcast_doc_delta` / `pump_collab_presence`). No mock: the
    // production event handlers do the writing and the real CRDT does the
    // merging, so this is a faithful test of live multi-peer chat.

    /// A distinct participant: a fresh app bound to the shared `ws_home`
    /// starter, authoring under `id` — genuinely different from any other peer.
    fn peer(id: &str, name: &str, initials: &str, color: usize) -> ChatApp {
        let mut app = ChatApp::new_ephemeral();
        app.me = crate::identity::LocalIdentity {
            id: id.into(),
            name: name.into(),
            initials: initials.into(),
            color_idx: color,
        };
        app.attach_framework(ui::framework::Dispatcher::detached("so.tilo.chat", id));
        app.active_ws = "ws_home".into();
        app.active_channel = "c_general".into();
        app.thread_open = None;
        app.focus = Focus::Main;
        app
    }

    /// Ship each peer's pending changes to the other — the broker fan-out.
    fn exchange(a: &mut AutoCommit, b: &mut AutoCommit) {
        let da = a.save_incremental();
        if !da.is_empty() {
            b.load_incoming(&da).unwrap();
        }
        let db = b.save_incremental();
        if !db.is_empty() {
            a.load_incoming(&db).unwrap();
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one labelled phase per original scenario, each with its own fresh fixture"
    )]
    fn two_participants_converge_on_messages_and_edits() {
        // Phase: messages_channels_and_reactions_converge_across_two_participants.
        {
            // Alice seeds a starter workspace; Bob adopts her document (as a
            // late-joining peer does) and announces himself.
            let mut doc_a = AutoCommit::new();
            crate::testing::starter(&mut doc_a, "u_alice", "Alice", "AL");
            let snapshot = doc_a.save(); // resets alice's incremental cursor
            let mut doc_b = AutoCommit::load(&snapshot).unwrap();
            crate::testing::introduce(&mut doc_b, "u_bob", "Bob", "BO");
            exchange(&mut doc_a, &mut doc_b);

            let mut a = peer("u_alice", "Alice", "AL", 0);
            let mut b = peer("u_bob", "Bob", "BO", 1);
            // The room's message parts, which both hold.
            let parts = crate::testing::Room::default();
            a.attach_room(&parts);
            b.attach_room(&parts);
            a.reload(&doc_a);
            b.reload(&doc_b);
            assert_eq!(a.me.id, "u_alice");
            assert_eq!(b.me.id, "u_bob");

            // Structure converges: a channel Alice creates shows up for Bob.
            crate::testing::apply_as(
                &mut doc_a,
                "u_alice",
                crate::resource::Chat::CREATE_CHANNEL.with(chat::CreateChannel {
                    workspace: item("ws_home"),
                    name: "planning".into(),
                    category: Some(item("cat_home_text")),
                    kind: None,
                    topic: None,
                }),
            )
            .unwrap();
            let chan = crate::testing::channels(&doc_a)
                .into_iter()
                .find(|channel| channel.name == "planning")
                .expect("the channel is made")
                .id;
            exchange(&mut doc_a, &mut doc_b);
            b.reload(&doc_b);
            assert!(
                b.channels
                    .iter()
                    .any(|c| c.id == chan && c.name == "planning"),
                "peer B sees the channel peer A created"
            );

            // Alice sends a message — it reaches Bob, attributed to Alice (not a
            // shared `u_me`).
            a.insert_text(&mut ui::widget::Edits::lend(&mut doc_a), "morning all");
            a.send_focused(&mut ui::widget::Edits::lend(&mut doc_a));
            exchange(&mut doc_a, &mut doc_b);
            b.reload(&doc_b);
            let alice_msg = b
                .messages
                .iter()
                .find(|m| m.text == "morning all")
                .expect("B sees A's message");
            assert_eq!(alice_msg.author, "u_alice");
            assert_eq!(
                b.user_name("u_alice"),
                "Alice",
                "attributed to the right person"
            );
            let msg_id = alice_msg.id.clone();

            // Bob replies — converges back to Alice, attributed to Bob.
            b.insert_text(&mut ui::widget::Edits::lend(&mut doc_b), "morning!");
            b.send_focused(&mut ui::widget::Edits::lend(&mut doc_b));
            exchange(&mut doc_a, &mut doc_b);
            a.reload(&doc_a);
            let bob_msg = a
                .messages
                .iter()
                .find(|m| m.text == "morning!")
                .expect("A sees B's reply");
            assert_eq!(bob_msg.author, "u_bob");
            assert_eq!(a.user_name("u_bob"), "Bob");

            // Both react with the SAME emoji to Alice's message. Distinct
            // identities mean the count is 2 — the exact thing a shared `u_me`
            // would have collapsed to 1.
            a.on_hit(
                Hit::Reaction {
                    msg: msg_id.clone(),
                    emoji: "👍".into(),
                },
                &mut ui::widget::Edits::lend(&mut doc_a),
            );
            b.on_hit(
                Hit::Reaction {
                    msg: msg_id.clone(),
                    emoji: "👍".into(),
                },
                &mut ui::widget::Edits::lend(&mut doc_b),
            );
            exchange(&mut doc_a, &mut doc_b);
            a.reload(&doc_a);
            b.reload(&doc_b);
            for (who, app) in [("A", &a), ("B", &b)] {
                let m = app.messages.iter().find(|m| m.id == msg_id).unwrap();
                let thumbs = m
                    .reactions
                    .iter()
                    .find(|r| r.emoji == "👍")
                    .expect("👍 present");
                assert_eq!(
                    thumbs.count(),
                    2,
                    "peer {who}: both distinct reactors counted once each"
                );
                assert!(thumbs.reacted_by("u_alice") && thumbs.reacted_by("u_bob"));
            }

            // Full convergence: identical timelines on both peers.
            let ids = |app: &ChatApp| {
                app.messages
                    .iter()
                    .map(|m| m.id.clone())
                    .collect::<Vec<_>>()
            };
            assert_eq!(ids(&a), ids(&b), "both peers converge on the same timeline");
        }

        // Phase: an_edit_and_its_history_converge_across_two_participants.
        {
            // The author edits a message; the *new* body AND the archived prior
            // version both reach the other peer. Drives the real inline-editor path
            // (begin_edit → insert_text → commit_edit → edit-message), then
            // shuttles the incremental delta a peer would receive from the broker.
            let mut doc_a = AutoCommit::new();
            crate::testing::starter(&mut doc_a, "u_alice", "Alice", "AL");
            let snapshot = doc_a.save();
            let mut doc_b = AutoCommit::load(&snapshot).unwrap();
            crate::testing::introduce(&mut doc_b, "u_bob", "Bob", "BO");
            exchange(&mut doc_a, &mut doc_b);

            let mut a = peer("u_alice", "Alice", "AL", 0);
            let mut b = peer("u_bob", "Bob", "BO", 1);
            // The room's message parts, which both hold.
            let parts = crate::testing::Room::default();
            a.attach_room(&parts);
            b.attach_room(&parts);
            a.reload(&doc_a);
            b.reload(&doc_b);

            // Alice posts, then Bob sees the original body.
            a.insert_text(&mut ui::widget::Edits::lend(&mut doc_a), "shipping friday");
            a.send_focused(&mut ui::widget::Edits::lend(&mut doc_a));
            exchange(&mut doc_a, &mut doc_b);
            b.reload(&doc_b);
            let msg_id = b
                .messages
                .iter()
                .find(|m| m.text == "shipping friday")
                .expect("B sees it")
                .id
                .clone();
            assert!(
                b.message(&msg_id).is_some_and(|m| !m.edited),
                "not edited yet"
            );

            // Alice edits through the real inline editor: load her own message,
            // append to its working text, and commit.
            a.begin_edit(msg_id.clone());
            assert!(a.editing.is_some(), "the author can open the inline editor");
            a.insert_text(
                &mut ui::widget::Edits::lend(&mut doc_a),
                " → moved to monday",
            );
            a.commit_edit(&mut ui::widget::Edits::lend(&mut doc_a));

            // The edit — new body + archived prior version — converges to Bob.
            exchange(&mut doc_a, &mut doc_b);
            b.reload(&doc_b);
            let m = b.message(&msg_id).expect("edited message present for B");
            assert_eq!(
                m.text, "shipping friday → moved to monday",
                "B sees the edited body"
            );
            assert!(m.edited, "B sees the (edited) flag");
            assert_eq!(m.history.len(), 1, "B sees one archived prior version");
            assert_eq!(
                m.history[0].text, "shipping friday",
                "the prior body is preserved verbatim"
            );

            // A second edit accumulates history without losing the first — proving
            // the log grows rather than overwriting.
            a.begin_edit(msg_id.clone());
            a.insert_text(&mut ui::widget::Edits::lend(&mut doc_a), " (confirmed)");
            a.commit_edit(&mut ui::widget::Edits::lend(&mut doc_a));
            exchange(&mut doc_a, &mut doc_b);
            b.reload(&doc_b);
            let m = b.message(&msg_id).unwrap();
            assert_eq!(m.history.len(), 2, "both prior versions are kept");
            assert_eq!(m.history[0].text, "shipping friday");
            assert_eq!(m.history[1].text, "shipping friday → moved to monday");
            // And a non-author cannot edit someone else's message via the same path.
            b.begin_edit(msg_id.clone());
            assert!(
                b.editing.is_none(),
                "a non-author can't open the editor on A's message"
            );
        }
    }

    #[test]
    fn a_view_only_peer_cannot_post_react_or_hydrate_embeds() {
        use ui::permissions::{PermissionContext, Role};
        let (mut app, mut doc) = app_with_doc();
        // Restrict this peer to a read-only member with no file capabilities —
        // exactly what an invite without write/file access produces.
        let ctx = PermissionContext::member(model::ME, Role::Viewer);
        app.perms = crate::permissions::ChatPermissions::from_context(ctx);

        assert!(
            !app.can_participate(),
            "a member without admin can't write to the workspace"
        );
        render_frame(&mut app, 1280.0, 720.0);
        assert_eq!(
            count_hits(&app, |h| matches!(
                h,
                Hit::FocusMain | Hit::Send | Hit::Attach
            )),
            0,
            "a viewer sees an honest non-interactive composer, not controls that fail silently"
        );
        assert!(
            !app.embed_allowed("/var/workspace/secret.png"),
            "an ungranted embed is gated"
        );

        // Sending is a no-op under the gate.
        let before = crate::testing::messages(&doc, "c_crit").len();
        app.insert_text(&mut ui::widget::Edits::lend(&mut doc), "should not post");
        app.send_focused(&mut ui::widget::Edits::lend(&mut doc));
        app.begin_edit("m_me1".into());
        assert!(
            app.editing.is_none(),
            "a viewer cannot open an editor for an existing message"
        );
        assert_eq!(
            crate::testing::messages(&doc, "c_crit").len(),
            before,
            "gated peer posts nothing"
        );

        // Attaching is a no-op too.
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("x.png");
        write_real_png(&png, 32, 32);
        assert!(
            !app.attach_file(&mut ui::widget::Edits::lend(&mut doc), &png),
            "a gated peer can't attach either"
        );

        // And reacting is gated.
        if let Some(id) = app
            .messages
            .iter()
            .find(|m| m.reactions.is_empty())
            .map(|m| m.id.clone())
        {
            app.on_hit(
                Hit::Reaction {
                    msg: id.clone(),
                    emoji: "👍".into(),
                },
                &mut ui::widget::Edits::lend(&mut doc),
            );
            let after = app.messages.iter().find(|mm| mm.id == id).unwrap();
            assert!(
                after.reactions.iter().all(|r| r.emoji != "👍"),
                "gated: no reaction added"
            );
        }
    }

    #[test]
    fn two_peers_join_through_native_service_without_publishing_legacy_tickets() {
        let mut doc_a = AutoCommit::new();
        crate::testing::starter(&mut doc_a, "u_alice", "Alice", "AL");
        let mut doc_b = AutoCommit::load(&doc_a.save()).unwrap();
        let mut a = peer("u_alice", "Alice", "AL", 0);
        let mut b = peer("u_bob", "Bob", "BO", 1);
        a.reload(&doc_a);
        b.reload(&doc_b);
        let before_a = doc_a.save();
        let before_b = doc_b.save();
        a.on_hit(
            Hit::Channel("c_lounge".into()),
            &mut ui::widget::Edits::lend(&mut doc_a),
        );
        b.on_hit(
            Hit::Channel("c_lounge".into()),
            &mut ui::widget::Edits::lend(&mut doc_b),
        );
        for app in [&mut a, &mut b] {
            assert!(app.call.muted);
            assert_eq!(
                app.native_voice.snapshot.state,
                ui::native_media::State::Preparing
            );
            assert_eq!(app.native_voice.take_requests(ui::time::now()).len(), 1);
            assert!(app.voice_in("c_lounge").is_empty());
        }
        assert_eq!(doc_a.save(), before_a);
        assert_eq!(doc_b.save(), before_b);
    }
}
