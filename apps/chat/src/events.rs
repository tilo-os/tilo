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

use crate::actions;
use crate::app::{
    AttachmentImportPending, AttachmentImportResult, AttachmentPickerResult, ChatApp, Focus, Hit,
    MenuAction, MenuTarget, RowSwipe,
};
use crate::doc::{ChatDoc, NOW};
use crate::model::{ChannelKind, Id};

/// Mint a fresh, collision-resistant message id at authoring time. Carried in
/// the [`ChatAction::SendMessage`](crate::actions::ChatAction) payload so the
/// insert replays byte-identically on every peer.
fn new_message_id() -> String {
    format!("m_{}", uuid::Uuid::new_v4().simple())
}

/// Mint a fresh channel id at authoring time (carried in the create action).
pub(crate) fn new_channel_id() -> String {
    format!("c_{}", uuid::Uuid::new_v4().simple())
}

/// Mint a category id before dispatch.  The id belongs in the collaborative
/// payload; generating it inside a document helper makes replay depend on a
/// different peer's RNG.
fn new_category_id() -> String {
    format!("cat_{}", uuid::Uuid::new_v4().simple())
}

fn now_secs() -> i64 {
    ui::time::try_unix_now().map_or(NOW, |secs| secs as i64)
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
fn drive_text_input(
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
        self.thread_open.is_some()
            && !self.emoji_open
            && matches!(
                event,
                WindowEvent::CursorMoved { .. }
                    | WindowEvent::MouseInput { .. }
                    | WindowEvent::MouseWheel { .. }
            )
            && drive_retained_control(&mut self.thread_scroll, event, scale, modifiers, doc)
    }

    pub(crate) fn dispatch_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        doc: &mut ui::widget::Edits<'_>,
        handled: &mut bool,
    ) -> bool {
        if self.route_context_menu_event(event, scale, modifiers, doc) {
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
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * 44.0,
                    MouseScrollDelta::PixelDelta(p) => p.y / scale,
                };
                if self.emoji_open && self.emoji_grid.contains(self.last_mouse) {
                    // Clamped against the content height when painted.
                    self.emoji_scroll = (self.emoji_scroll - dy).max(0.0);
                    *handled = true;
                    return false;
                }
                if self.thread_open.is_some() && self.thread_body.contains(self.last_mouse) {
                    // At a thread boundary, do not bubble wheel input to the
                    // hidden/main timeline behind the detail view.
                    *handled = true;
                    return false;
                }
                self.scroll += dy;
                // A positive offset moves away from the live edge. Do this at
                // event time rather than waiting for paint: `sync` may run
                // between the input event and the next frame, and must not
                // acknowledge messages the reader has just scrolled away from.
                if dy > 0.0 {
                    self.timeline_at_end = false;
                }
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
        self.forward_move_to_text_inputs(event, scale, modifiers, doc);
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
        // End any composer drag-selection in progress.
        self.forward_release_to_text_inputs(event, scale, modifiers, doc);
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
        // An `@name` being typed in a composer takes Tab to complete it.
        if matches!(logical_key, Key::Named(NamedKey::Tab))
            && self.editing.is_none()
            && self.rename.is_none()
            && self.focused_composer().is_focused()
            && self.complete_mention_on_tab(event, modifiers, doc)
        {
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

        let composer_owns = self.can_participate()
            && self.menu.is_none()
            && !self.emoji_open
            && self.history_open.is_none()
            && !self.attach_open
            && !matches!(logical_key, Key::Named(NamedKey::Escape));
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
        } else if self.can_participate()
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
    /// thread draft `Text` in the CRDT (diff-spliced) so a draft stays
    /// collaborative + persistent. On an empty composer, `ArrowUp` loads your
    /// last message into the inline editor (Slack/Discord convention).
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
        // The draft is created the first time its surface is written in.
        let key = self.draft_key();
        if ChatDoc::rich_draft(doc, &key).is_none() {
            self.ensure_composer_draft(doc, &key);
            self.focused_composer_mut().sync(doc);
        }
        self.focused_composer_mut()
            .handle(event, scale, modifiers, doc);
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

    /// Tab while an `@name` is being typed completes it with the best match
    /// in the room, as a mention. Returns whether it did.
    fn complete_mention_on_tab(
        &mut self,
        event: &WindowEvent,
        modifiers: Modifiers,
        doc: &mut ui::widget::Edits<'_>,
    ) -> bool {
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Named(NamedKey::Tab),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };
        if modifiers.state().shift_key() {
            return false;
        }
        let Some(before) = self.focused_composer().text_before_caret() else {
            return false;
        };
        let participants = self.participants();
        let Some(query) = crate::mention::picker_query(&before, before.len(), &participants) else {
            return false;
        };
        let Some(who) = crate::mention::candidates(&participants, &query.query)
            .first()
            .map(|who| (*who).clone())
        else {
            return false;
        };
        let typed = before.len().saturating_sub(query.start);
        let label = format!("@{}", who.name);
        self.focused_composer_mut()
            .complete_mention(doc, typed, &who.id, &label);
        true
    }

    /// Forward a `CursorMoved` to every text input so hover and drag-select
    /// state stays current regardless of which surface is focused.
    fn forward_move_to_text_inputs(
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

    /// Forward a mouse release to every text input so drag-selection ends.
    fn forward_release_to_text_inputs(
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
            Hit::MessageMenu { msg } => Some(self.context_menu(
                MenuTarget::Message(msg),
                self.last_mouse,
                MenuAction::for_message().to_vec(),
            )),
            _ => Some(self.context_menu(
                MenuTarget::Workspace(self.active_ws.clone()),
                self.last_mouse,
                vec![MenuAction::NewCategory],
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
                self.inbox_open = true;
                self.thread_open = None;
                self.focus = Focus::Main;
            }
            Hit::CurrentContext => {
                self.inbox_open = false;
                self.on_hit_workspace(self.context_ws.clone());
            }
            Hit::Channel(id) => self.on_hit_channel(id, doc),
            Hit::VoiceChannel(id) => self.join_voice(&id, doc),
            Hit::CategoryToggle(id) => self.toggle_category(&id),
            Hit::EditSave => self.commit_edit(doc),
            Hit::EditCancel => self.cancel_edit(),
            Hit::AddChannel {
                workspace,
                category,
            } if self.can_participate() => self.on_hit_add_channel(&workspace, &category, doc),
            Hit::AddCategory(ws) if self.can_participate() => self.on_hit_add_category(&ws, doc),
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
            Hit::Send => self.send_focused(doc),
            Hit::JumpToLatest => self.jump_to_latest(),
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
            Hit::StartCall if self.call_allows(crate::call::CallAction::Join) => {
                self.on_hit_start_call(doc);
            }
            Hit::StopSharingLiveContext => {
                // The same device-local handler the public effect reaches.
                let _ = self.answer_effect(crate::effects::ChatEffect::StopSharingLiveContext);
            }
            Hit::CopyResourceLink(message) => {
                if let Some(dispatch) = &self.dispatch {
                    dispatch.effect(&crate::effects::ChatEffect::CopyResourceLink { message });
                }
            }
            Hit::OpenResourceLink(message) => {
                if let Some(dispatch) = &self.dispatch {
                    dispatch.effect(&crate::effects::ChatEffect::OpenResourceLink { message });
                }
            }
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
            Hit::ToggleRightFlap => self.right_flap.toggle(),
            Hit::Dismiss => self.on_hit_dismiss(),
            // MessageMenu is a right-click affordance and CallDragHandle is
            // handled at press time; permission-gated fallthroughs are no-ops.
            _ => {}
        }
        self.reload(doc);
    }

    fn on_hit_workspace(&mut self, id: Id) {
        if self.active_ws != id {
            self.active_ws = id;
            self.thread_open = None;
            self.reset_timeline_to_latest();
            self.focus = Focus::Main;
            self.active_channel.clear(); // force reload to pick first text channel
        }
    }

    fn on_hit_channel(&mut self, id: Id, doc: &mut ui::widget::Edits<'_>) {
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

    fn on_hit_add_channel(
        &mut self,
        workspace: &str,
        category: &str,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        let id = new_channel_id();
        self.apply_action(
            doc,
            &actions::create_channel_action(
                workspace,
                category,
                &id,
                "new-channel",
                ChannelKind::Text,
            ),
        );
        self.begin_rename(MenuTarget::Channel(id));
    }

    fn on_hit_add_category(&mut self, ws: &str, doc: &mut ui::widget::Edits<'_>) {
        let id = new_category_id();
        self.apply_action(
            doc,
            &actions::create_category_action(ws, &id, "New category"),
        );
        self.begin_rename(MenuTarget::Category(id));
    }

    fn on_hit_reaction(&mut self, msg: &str, emoji: &str, doc: &mut ui::widget::Edits<'_>) {
        if self.can_participate() {
            let action = self.reaction_toggle_action(msg, emoji);
            self.apply_action(doc, &action);
            self.reload(doc);
        }
    }

    fn on_hit_open_thread(&mut self, id: Id) {
        if self.thread_open.as_ref() != Some(&id) {
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
                self.apply_action(doc, &action);
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
                    let channel = self.active_channel.clone();
                    let pinned = self.message(&id).is_some_and(|m| m.pinned);
                    self.apply_action(doc, &actions::pin_message_action(&channel, &id, !pinned));
                }
                MenuAction::DeleteMessage => {
                    let channel = self.active_channel.clone();
                    self.apply_action(doc, &actions::delete_message_action(&channel, &id));
                }
                _ => {}
            },
            MenuTarget::Workspace(ws) => {
                if action == MenuAction::NewCategory {
                    let id = new_category_id();
                    self.apply_action(
                        doc,
                        &actions::create_category_action(&ws, &id, "New category"),
                    );
                    self.begin_rename(MenuTarget::Category(id));
                }
            }
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
            MenuAction::Delete => {
                self.apply_action(doc, &actions::delete_channel_action(id));
                if self.active_channel == id {
                    self.active_channel.clear(); // reload picks a new one
                }
            }
            MenuAction::Archive => {
                let archived = self.channel(id).is_some_and(|c| c.archived);
                self.apply_action(doc, &actions::archive_channel_action(id, !archived));
                if !archived && self.active_channel == id {
                    self.active_channel.clear(); // an archived channel leaves the view
                }
            }
            MenuAction::MoveUp => {
                self.apply_action(doc, &actions::reorder_channel_action(id, true));
            }
            MenuAction::MoveDown => {
                self.apply_action(doc, &actions::reorder_channel_action(id, false));
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
            MenuAction::Delete => self.apply_action(doc, &actions::delete_category_action(id)),
            MenuAction::ToggleCollapse => self.toggle_category(id),
            MenuAction::NewChannelHere => {
                let new_id = new_channel_id();
                self.apply_action(
                    doc,
                    &actions::create_channel_action(
                        &ws,
                        id,
                        &new_id,
                        "new-channel",
                        ChannelKind::Text,
                    ),
                );
                self.begin_rename(MenuTarget::Channel(new_id));
            }
            MenuAction::NewVoiceChannelHere => {
                let new_id = new_channel_id();
                self.apply_action(
                    doc,
                    &actions::create_channel_action(
                        &ws,
                        id,
                        &new_id,
                        "New Voice",
                        ChannelKind::Voice,
                    ),
                );
                self.begin_rename(MenuTarget::Channel(new_id));
            }
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
                        self.apply_action(doc, &actions::rename_channel_action(&id, &text));
                    }
                    MenuTarget::Category(id) => {
                        self.apply_action(doc, &actions::rename_category_action(&id, &text));
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

    /// The draft text the focused composer was last loaded from (by
    /// `reload`), the base its next edit is spliced against.
    fn draft_key(&self) -> String {
        match self.focus {
            Focus::Thread => self
                .thread_draft_key()
                .unwrap_or_else(|| self.active_channel.clone()),
            Focus::Main => self.active_channel.clone(),
        }
    }

    /// Insert `s` at the active shared input's caret. Message edits remain in
    /// memory until saved; otherwise this targets the focused composer
    /// `TextInput` — used by the emoji picker, which splices a picked glyph as
    /// if typed. Composer keystrokes themselves go straight to the widget via
    /// [`route_composer_key`].
    fn insert_text(&mut self, doc: &mut ui::widget::Edits<'_>, s: &str) {
        if !self.can_participate() {
            return;
        }
        if self.editing.is_some() {
            self.edit_input.insert_text(doc, s);
            return;
        }
        let key = self.draft_key();
        if ChatDoc::rich_draft(doc, &key).is_none() {
            self.ensure_composer_draft(doc, &key);
            self.focused_composer_mut().sync(doc);
        }
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

    /// Commit the inline message editor: write the new body through
    /// [`ChatDoc::edit_message`] (which archives the prior text into the
    /// message's history and flags it edited), then close the editor. An
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
        let channel = self.active_channel.clone();
        self.apply_action(
            doc,
            &actions::edit_message_action(&channel, &id, &text, now_secs()).with_body(body),
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
        // Posting is a write into the workspace: gated by the OS permission
        // context (a View-only / ungranted peer can read but not send).
        if !self.can_participate() {
            return;
        }
        let (key, parent) = match self.focus {
            Focus::Thread => match self.thread_draft_key() {
                Some(key) => (key, self.thread_open.clone()),
                None => (self.active_channel.clone(), None),
            },
            Focus::Main => (self.active_channel.clone(), None),
        };
        let text = ChatDoc::draft_text(doc, &key);
        if text.trim().is_empty() {
            return;
        }
        let body = ChatDoc::draft_clips(doc, &key);
        let blocks = ChatDoc::draft_blocks(doc, &key);
        // The message ROW insert is the action: id + timestamp minted here at
        // authoring time and carried in the payload, dispatched through the same
        // executor path Ctrl+K uses (undo boundary + byte-identical replay). The
        // draft clear is presentation (ephemeral per-channel composer state) and
        // stays a direct write inside the same event.
        let channel = self.active_channel.clone();
        // Bind `@name` to participants once, here, at authoring time. The
        // message carries the resolved references, so no reader — a peer, a
        // notification, an agent watching the room — ever re-parses the body to
        // work out who it was addressed to.
        let mentions = crate::body::mentions(&blocks, &self.participants());
        self.apply_action(
            doc,
            &actions::send_message_action(
                &channel,
                &new_message_id(),
                now_secs(),
                &text,
                parent,
                None,
                mentions,
            )
            .with_body(body),
        );
        self.apply_action(doc, &actions::ChatAction::ClearDraft { key });
        self.focused_composer_mut().sync(doc);
        if matches!(self.focus, Focus::Main) {
            self.jump_to_latest();
        }
        self.reload(doc);
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
            let result = if crate::picker::folder_readable(&dir) {
                AttachmentPickerResult::Ready {
                    generation,
                    entries: crate::picker::list_attachments(&dir),
                }
            } else {
                AttachmentPickerResult::Failed {
                    generation,
                    reason: "workspace is unavailable".into(),
                }
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
        let generation = match &result {
            AttachmentPickerResult::Ready { generation, .. }
            | AttachmentPickerResult::Failed { generation, .. } => *generation,
        };
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
        match result {
            AttachmentPickerResult::Ready { entries, .. } => {
                self.attach_entries = entries;
                self.attachment_status = None;
            }
            AttachmentPickerResult::Failed { reason, .. } => {
                self.attachment_status = Some(format!("Attachment picker failed: {reason}"));
            }
        }
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
            self.attachment_status = Some(format!("Attachment import failed: {reason}"));
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
        self.submit_action(&actions::send_message_action(
            &pending.channel,
            &new_message_id(),
            now_secs(),
            "",
            None,
            Some(attachment),
            Vec::new(),
        ));
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
        return fail("file is unavailable");
    };
    let Ok(metadata) = tilo_files::blocking::metadata(&dir, &name) else {
        return fail("file is unavailable");
    };
    if metadata.kind != tilo_files::Kind::File || metadata.len > crate::doc::MAX_ATTACHMENT_BYTES {
        return fail("file is not shareable or exceeds 32 MiB");
    }
    let Ok(bytes) = tilo_files::blocking::read(&dir, &name) else {
        return fail("could not read file");
    };
    if bytes.is_empty() {
        return fail("file is empty");
    }
    if u64::try_from(bytes.len())
        .ok()
        .is_none_or(|size| size > crate::doc::MAX_ATTACHMENT_BYTES)
    {
        return fail("file exceeds 32 MiB");
    }
    let name = path
        .file_name()
        .map_or_else(|| "file".into(), |name| name.to_string_lossy().into_owned());
    let meta = tilo_content::sniff(&bytes);
    let blob = match content.put(&bytes) {
        Ok(blob) => blob,
        Err(error) => {
            eprintln!("chat: content store refused an attachment: {error}");
            return fail("shared content store refused the attachment");
        }
    };
    if content.read(&blob).as_deref() != Some(bytes.as_slice()) {
        return fail("shared byte verification failed");
    }
    AttachmentImportResult::Ready {
        token,
        name: name.clone(),
        blob,
        size: u64::try_from(bytes.len()).unwrap_or(crate::doc::MAX_ATTACHMENT_BYTES),
        width: meta.w,
        height: meta.h,
        video: crate::picker::is_video_name(&name),
    }
}

/// Run a bounded job on its own thread, or inline where no thread can start
/// (a browser page, where the job's workspace reads fail fast). A native
/// thread that cannot start leaves the reason on the status line.
fn run_worker(name: &str, job: impl FnOnce() + Send + 'static, status: &mut Option<String>) {
    if ui::rt::availability(ui::rt::Feature::Threads) != ui::rt::Availability::Available {
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
    use crate::doc::{ChatDoc, NewMessage};
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

    fn app_with_doc() -> (ChatApp, AutoCommit) {
        let mut doc = AutoCommit::new();
        ChatDoc::seed_if_empty(&mut doc);
        let mut app = ChatApp::new_ephemeral();
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
            MenuTarget::Workspace(_) => vec![MenuAction::NewCategory],
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
                "chat.channel:c_crit",
            )
            .expect("c_crit is a channel resource");
            let spec = crit.actions.iter().find(|a| a.id == "delete").unwrap();
            let from_spec = match &spec.input {
                tilo_actions::ActionInput::Immediate(e) => e.as_ref().clone(),
                tilo_actions::ActionInput::Prompted { .. }
                | tilo_actions::ActionInput::Deferred { .. } => {
                    panic!("delete is an immediate action")
                }
            };

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

            assert_eq!(dispatched.type_id, from_spec.type_id);
            assert_eq!(
                dispatched.payload, from_spec.payload,
                "the delete-channel affordance and its Ctrl+K spec build the identical action"
            );
            // And it really deleted the channel (the affordance is on the real path).
            assert!(
                ChatDoc::list_channels(&doc)
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
            assert_eq!(menu.children[0].name.as_deref(), Some("Rename channel"));
            assert_eq!(menu.children[1].name.as_deref(), Some("Archive channel"));
            assert_eq!(
                menu.children[4].name.as_deref(),
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
                matches!(&app.rename, Some(MenuTarget::Channel(id)) if id == "c_random"),
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
            assert!(
                !app.channels.iter().any(|c| c.id == "c_random"),
                "the committed swipe deleted the channel"
            );
        }
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
            ChatDoc::set_draft(&mut doc, "c_general", "hi from the composer");
            app.reload(&doc);
            app.on_hit(Hit::Send, &mut ui::widget::Edits::lend(&mut doc));
            let action = app
                .dispatched
                .borrow()
                .iter()
                .find(|action| matches!(action, crate::actions::ChatAction::SendMessage { .. }))
                .cloned()
                .expect("send dispatched a message");
            match action {
                crate::actions::ChatAction::SendMessage { channel, text, .. } => {
                    assert_eq!(channel, "c_general");
                    assert_eq!(text, "hi from the composer");
                }
                other => panic!("expected SendMessage, got {other:?}"),
            }
        }

        // Phase: typing_then_send_appends_a_message.
        {
            let (mut app, mut doc) = app_with_doc();
            let before = ChatDoc::list_messages(&doc, "c_crit").len();
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
            let after = ChatDoc::list_messages(&doc, "c_crit");
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
            let replies = model::reply_count(&ChatDoc::list_messages(&doc, "c_crit"), "m_concept");
            assert_eq!(
                replies, 4,
                "thread gained one reply on top of the seeded three"
            );
        }
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
        let before = ChatDoc::top_level_count(&doc, "c_general");
        ChatDoc::set_draft(&mut doc, "c_general", "accepted semantic send");
        app.reload(&doc);
        render_frame(&mut app, 1280.0, 720.0);
        assert!(ui::Widget::perform_semantic_action(
            &mut app,
            &target,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!(ChatDoc::top_level_count(&doc, "c_general"), before + 1);
        assert_eq!(ChatDoc::draft_text(&doc, "c_general"), "");
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one labelled phase per original scenario, each with its own fresh fixture"
    )]
    fn live_start_workspace_switch_and_inbox_navigate_real_conversations() {
        // Phase: live_starter_selects_first_real_workspace_and_channel.
        {
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

            let before = ChatDoc::list_messages(&doc, "c_crit").len();
            app.on_hit(
                Hit::AttachPick(png.to_string_lossy().into_owned()),
                &mut ui::widget::Edits::lend(&mut doc),
            );
            assert_eq!(
                ChatDoc::list_messages(&doc, "c_crit").len(),
                before,
                "selecting only accepts a bounded local import; sync authors the message"
            );
            settle_attachment_until(&mut app, &mut doc, "c_crit", before + 1);
            let msgs = ChatDoc::list_messages(&doc, "c_crit");
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
                .try_send(AttachmentPickerResult::Failed {
                    generation: 6,
                    reason: "old worker".into(),
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
                .try_send(AttachmentPickerResult::Failed {
                    generation: 7,
                    reason: "workspace is unavailable".into(),
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
            assert!(!ChatDoc::list_messages(&doc, "c_crit").is_empty());
        }

        // Phase: attaching_a_video_posts_an_inline_video_card.
        {
            let (mut app, mut doc) = app_with_doc();
            let dir = tempfile::tempdir().unwrap();
            let mp4 = dir.path().join("clip.mp4");
            std::fs::write(&mp4, b"\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00moov").unwrap();
            let before = ChatDoc::list_messages(&doc, "c_crit").len();
            app.on_hit(
                Hit::AttachPick(mp4.to_string_lossy().into_owned()),
                &mut ui::widget::Edits::lend(&mut doc),
            );
            settle_attachment_until(&mut app, &mut doc, "c_crit", before + 1);
            let msgs = ChatDoc::list_messages(&doc, "c_crit");
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
            let before = ChatDoc::list_messages(&doc, "c_crit").len();

            app.on_hit(
                Hit::AttachPick(note.to_string_lossy().into_owned()),
                &mut ui::widget::Edits::lend(&mut doc),
            );
            settle_attachment_until(&mut app, &mut doc, "c_crit", before + 1);
            match &ChatDoc::list_messages(&doc, "c_crit")
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

    #[test]
    fn channels_and_categories_are_created_renamed_and_reordered_from_the_sidebar() {
        // Phase: add_channel_creates_and_opens_rename.
        {
            let (mut app, mut doc) = app_with_doc();
            let before = app.channels.len();
            app.on_hit(
                Hit::AddChannel {
                    workspace: "ws_acme".into(),
                    category: "cat_acme_main".into(),
                },
                &mut ui::widget::Edits::lend(&mut doc),
            );
            assert_eq!(app.channels.len(), before + 1);
            assert!(
                app.rename.is_some(),
                "new channel opens the inline rename editor"
            );
        }

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
                ChatDoc::list_channels(&doc)
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
            app.on_hit(
                Hit::AddCategory("ws_acme".into()),
                &mut ui::widget::Edits::lend(&mut doc),
            );
            assert_eq!(acme_cats(&app), before + 1, "a category was created");
            assert!(
                app.rename.is_some(),
                "a new category opens the inline rename editor"
            );
            app.cancel_rename();
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
            app.rect = vello::kurbo::Rect::new(0.0, 0.0, 560.0, 720.0);
            app.reconcile_flaps();
            assert!(app.left_overlay(), "narrowing reflows to a drawer");
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
        doc: &mut AutoCommit,
        channel: &str,
        expected_count: usize,
    ) {
        tilo_test_env::wait_until("the attachment worker", || {
            app.settle_attachment_import();
            app.land(doc);
            ChatDoc::list_messages(doc, channel).len() == expected_count
        });
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
                ChatDoc::list_messages(&doc, "c_crit")
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

    fn stage_agent_work(app: &mut ChatApp, doc: &mut AutoCommit) {
        for (author, action) in crate::resource::agent_work_scenario() {
            crate::actions::apply_as(doc, author, &action).unwrap();
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
        assert!(state.adopt_doc(doc));
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
        assert!(state.adopt_doc(doc));
        let mut driver = FrameHarness::for_root(state, app, (960, 720), ui::Theme::dark());
        assert!(!driver.root::<ChatApp>().left_overlay());
        driver.resize((700, 720)).expect("normal narrow layout");
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
        let address = ui::tilo_actions::ActionAddress::new(resize.id.as_str(), "set-value");
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
        assert_eq!(root.left_flap.list_rect().width(), 212.0);
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
            "rail + sidebar + conversation detail fit at this width"
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
        render_frame(&mut app, 700.0, 720.0);
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
            app.sync(&doc, &automerge::ROOT);
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
                    .find(|node| node.name.as_deref() == Some("Tide: "))
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
                .find(|message| message.id == "m_agent_reply")
                .unwrap()
                .work
                .as_ref()
                .unwrap()
                .trace[0]
                .clone();
            applied_step.outcome = AgentStepOutcome::Applied;
            crate::actions::apply_as(
                &mut doc,
                "agent-task:11111111-1111-4111-8111-111111111111",
                &crate::actions::set_agent_work_action(
                    "c_crit",
                    "m_agent_reply",
                    AgentWork::new(
                        AgentWorkState::Done {
                            did: "Revenue curve added".into(),
                        },
                        crate::doc::NOW,
                    )
                    .with_trace(vec![applied_step]),
                    None,
                ),
            )
            .unwrap();
            app.land(&mut doc);
            app.sync(&doc, &automerge::ROOT);
            render_frame(&mut app, width, height);
            assert_eq!(
                description(&app),
                "Completed: Revenue curve added; Applied: Add function",
                "the state update retains the previously recorded dispatch evidence"
            );
            let reply = app
                .messages
                .iter()
                .find(|message| message.id == "m_agent_reply")
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
        app.sync(&doc, &automerge::ROOT);
        let work = AgentWork::new(AgentWorkState::Done { did: "Reviewed all operations".into() }, crate::doc::NOW)
            .with_trace((0..60).map(|index| AgentStep::authored("calculator.workspace",
                format!("Operation {index}: preserve graph bounds and add the requested revenue curve"),
                AgentStepOutcome::Applied, index)).collect());
        crate::actions::apply_as(
            &mut doc,
            "agent-task:11111111-1111-4111-8111-111111111111",
            &crate::actions::set_agent_work_action("c_crit", "m_agent_reply", work, None),
        )
        .unwrap();
        app.sync(&doc, &automerge::ROOT);
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
        WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state,
            button,
        }
    }
    fn wheel(y: f64) -> WindowEvent {
        WindowEvent::MouseWheel {
            device_id: winit::event::DeviceId::dummy(),
            delta: MouseScrollDelta::LineDelta(0.0, y as f32),
            phase: winit::event::TouchPhase::Moved,
        }
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
                root.children.iter().any(|node| {
                    node.name
                        .as_deref()
                        .is_some_and(|name| name.contains("current workspace"))
                }),
                "the OS context is explicitly named"
            );
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
            let (_, header_tip) = app
                .hover_tooltip_target(app.header_rect().center())
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
            let (call_rect, _) = app
                .hits
                .iter()
                .find(|(_, hit)| matches!(hit, Hit::StartCall))
                .expect("labelled call button");
            assert!(
                app.hover_tooltip_target(call_rect.center()).is_none(),
                "the header title must not leak through a foreground control"
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
                ChatDoc::draft_text(&doc, &app.active_channel),
                "🦀 Accessible draft",
                "semantic editing updates the same unsent collaborative draft"
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
        let sent = ChatDoc::list_messages(&doc, &app.active_channel)
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
        let sent = ChatDoc::list_messages(&doc, &app.active_channel)
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
        ChatDoc::set_draft(&mut doc, &app.active_channel, "Ae\u{301}👋B");
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
            let action: crate::actions::ChatAction = serde_json::from_slice(
                &app.last_dispatched()
                    .expect("the semantic control dispatches through ActionHost")
                    .payload,
            )
            .expect("the dispatched envelope contains a ChatAction");
            assert!(matches!(
                action,
                crate::actions::ChatAction::AddReaction { .. }
                    | crate::actions::ChatAction::RemoveReaction { .. }
            ));
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
        assert!(rows.iter().all(|(_, channel)| channel.is_none()));
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
            let fresh = ChatDoc::create_workspace(&mut doc, "Private draft");
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
            ChatDoc::seed_starter(&mut doc, model::ME, "You", "QD");
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
        ChatDoc::push_message(
            &mut doc,
            &channel,
            NewMessage {
                author: "u_aria".into(),
                ts: NOW + 1,
                text: "arrived while nobody was looking".into(),
                ..Default::default()
            },
        );
        let watermark = ChatDoc::read_watermark(&doc, &channel, &app.me.id);
        let heads = doc.get_heads();
        for _ in 0..5 {
            assert!(!app.update(&doc, &automerge::ROOT));
            app.sync(&doc, &automerge::ROOT);
        }
        assert_eq!(
            ChatDoc::read_watermark(&doc, &channel, &app.me.id),
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
            app.update(&doc, &automerge::ROOT),
            "first presentation re-runs sync"
        );
        app.sync(&doc, &automerge::ROOT);
        app.land(&mut doc);
        assert!(
            ChatDoc::read_watermark(&doc, &channel, &app.me.id) > watermark,
            "the presented channel is marked read"
        );
        assert!(
            !app.update(&doc, &automerge::ROOT),
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
            ChatDoc::push_message(
                &mut doc,
                "c_random",
                NewMessage {
                    author: "u_aria".into(),
                    ts: NOW,
                    text: "anyone around?".into(),
                    ..Default::default()
                },
            );
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
            let watermark = ChatDoc::read_watermark(&doc, &app.active_channel, &app.me.id);

            // A remote message arrives after the current read boundary. Being in
            // the room is not enough to acknowledge it once the reader has moved
            // back through history.
            ChatDoc::push_message(
                &mut doc,
                &app.active_channel,
                NewMessage {
                    author: "u_aria".into(),
                    ts: NOW + 1,
                    text: "A newer update while you are reading history".into(),
                    ..Default::default()
                },
            );
            app.timeline_at_end = false;
            app.mark_active_read(&doc);
            app.land(&mut doc);
            assert_eq!(
                ChatDoc::read_watermark(&doc, &app.active_channel, &app.me.id),
                watermark,
                "a reader away from the live edge does not send a false read receipt"
            );

            app.jump_to_latest();
            app.mark_active_read(&doc);
            app.land(&mut doc);
            assert!(
                ChatDoc::read_watermark(&doc, &app.active_channel, &app.me.id) > watermark,
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
                ChatDoc::push_message(
                    &mut doc,
                    &app.active_channel,
                    NewMessage {
                        author: "u_aria".into(),
                        ts: NOW + i64::from(n),
                        text: format!(
                            "Longer history line {n}: enough text to wrap inside a narrow conversation column."
                        ),
                        ..Default::default()
                    },
                );
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
    #[allow(
        clippy::too_many_lines,
        reason = "one labelled phase per original scenario, each with its own fresh fixture"
    )]
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
        use ui::permissions::{Actor, PermissionContext, Role};
        let (mut app, mut doc) = app_with_doc();
        app.on_hit(
            Hit::Channel("c_huddle".into()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        app.land(&mut doc);
        app.reload(&doc);
        app.on_hit(Hit::CallExpand, &mut ui::widget::Edits::lend(&mut doc));
        app.perms = crate::permissions::ChatPermissions::from_context(
            PermissionContext::from_capabilities(
                Actor::person(Some(Role::Viewer), model::ME),
                vec![],
            ),
        );
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
            use crate::model::Presence;
            // Alice seeds a starter workspace; Bob adopts her document (as a
            // late-joining peer does) and announces himself.
            let mut doc_a = AutoCommit::new();
            ChatDoc::seed_starter(&mut doc_a, "u_alice", "Alice", "AL");
            let snapshot = doc_a.save(); // resets alice's incremental cursor
            let mut doc_b = AutoCommit::load(&snapshot).unwrap();
            ChatDoc::ensure_user(&mut doc_b, "u_bob", "Bob", "BO", 1, Presence::Online);
            exchange(&mut doc_a, &mut doc_b);

            let mut a = peer("u_alice", "Alice", "AL", 0);
            let mut b = peer("u_bob", "Bob", "BO", 1);
            a.reload(&doc_a);
            b.reload(&doc_b);
            assert_eq!(a.me.id, "u_alice");
            assert_eq!(b.me.id, "u_bob");

            // Structure converges: a channel Alice creates shows up for Bob.
            let chan = ChatDoc::create_channel(
                &mut doc_a,
                "ws_home",
                "cat_home_text",
                "planning",
                ChannelKind::Text,
            );
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
            use crate::model::Presence;
            // The author edits a message; the *new* body AND the archived prior
            // version both reach the other peer. Drives the real inline-editor path
            // (begin_edit → insert_text → commit_edit → ChatDoc::edit_message), then
            // shuttles the incremental delta a peer would receive from the broker.
            let mut doc_a = AutoCommit::new();
            ChatDoc::seed_starter(&mut doc_a, "u_alice", "Alice", "AL");
            let snapshot = doc_a.save();
            let mut doc_b = AutoCommit::load(&snapshot).unwrap();
            ChatDoc::ensure_user(&mut doc_b, "u_bob", "Bob", "BO", 1, Presence::Online);
            exchange(&mut doc_a, &mut doc_b);

            let mut a = peer("u_alice", "Alice", "AL", 0);
            let mut b = peer("u_bob", "Bob", "BO", 1);
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
        use ui::permissions::{Actor, PermissionContext, Role};
        let (mut app, mut doc) = app_with_doc();
        // Restrict this peer to a read-only member with no file capabilities —
        // exactly what an invite without write/file access produces.
        let ctx = PermissionContext::from_capabilities(
            Actor::person(Some(Role::Viewer), model::ME),
            vec![],
        );
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
        let before = ChatDoc::list_messages(&doc, "c_crit").len();
        app.insert_text(&mut ui::widget::Edits::lend(&mut doc), "should not post");
        app.send_focused(&mut ui::widget::Edits::lend(&mut doc));
        app.begin_edit("m_me1".into());
        assert!(
            app.editing.is_none(),
            "a viewer cannot open an editor for an existing message"
        );
        assert_eq!(
            ChatDoc::list_messages(&doc, "c_crit").len(),
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
        ChatDoc::seed_starter(&mut doc_a, "u_alice", "Alice", "AL");
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
