//! What a field tells whoever holds it: a submitting Enter, a committed edit, a settled
//! selection, and Escape letting go of what was typed.

use super::TextInput;
use tilo_ui_base::widget::ActionEmitter;

impl TextInput {
    /// Run `callback` with the text when the person presses Enter: the
    /// field's commit for a tree-owned field nobody polls. Enter submits
    /// (Shift+Enter still breaks a wrapping field's line).
    #[must_use]
    pub fn on_submit(mut self, callback: impl FnMut(&str) + 'static) -> Self {
        self.set_on_submit(Some(Box::new(callback)));
        self
    }

    /// Run `callback` on a submitting Enter (Shift+Enter still breaks a wrapping field's
    /// line), or, given none, let Enter break the line again.
    pub fn set_on_submit(&mut self, callback: Option<Box<dyn FnMut(&str)>>) {
        self.repaint = true;
        self.enter_submits = callback.is_some();
        self.on_submit = callback;
    }

    /// Route Enter through an exported resource action with the text as its
    /// input; see [`Self::on_submit`].
    #[must_use]
    /// Enter submits the field's action. A press whose submission the host has not collected yet is
    /// replaced by the next: two Enters delivered before the next frame submit once, so a repeated
    /// press cannot advance a wizard twice.
    pub fn on_submit_action(mut self, binding: tilo_actions::ActionBinding<String>) -> Self {
        self.enter_submits = true;
        self.submit_action = Some(ActionEmitter::new_replace_one(crate::Bind::bound(binding)));
        self
    }

    /// Report the selection each time it settles — a drag let go, a double or triple click, a
    /// touch release, a select all — as `<start>:<end>` byte offsets into the text, or nothing
    /// when the selection collapsed. A read-only field reports it so what reads it can act on
    /// the run someone chose (a message answered on just those words); a field being typed in
    /// keeps its selection private unless it is given one too.
    #[must_use]
    pub fn with_selection_event(mut self, event: tilo_ui_base::component::Event) -> Self {
        self.selection_event = Some(event);
        self
    }

    /// Tell `event` when Escape lets go of what is being typed: the field clears first.
    pub fn set_cancel_event(&mut self, event: tilo_ui_base::component::Event) {
        self.cancel_event = event;
    }

    /// Escape let go of the field: what holds it is told, and nothing typed is kept. `false`
    /// when nothing listens, so Escape is the field's as it always was.
    pub(crate) fn cancel(&mut self) -> bool {
        if self.cancel_event.is_none() {
            return false;
        }
        self.set_text("");
        self.cancel_event.fire(None);
        true
    }

    pub fn set_selection_event(&mut self, event: tilo_ui_base::component::Event) {
        self.selection_event = Some(event);
    }

    /// Tell `event` when the field's text goes from empty to non-empty, carrying what it holds
    /// now: the first change, which makes the item a list's entry names.
    pub fn set_begin_event(&mut self, event: tilo_ui_base::component::Event) {
        self.begin_event = event;
    }

    /// Tell `event` whenever the field's text changes as the person types, carrying what it holds
    /// now: what acts on the words while they are being typed, as a window's search does.
    pub fn set_changed_event(&mut self, event: tilo_ui_base::component::Event) {
        self.changed_event = event;
    }

    /// Tell `event` when the field loses the keyboard: an entry left empty is taken away by what
    /// listens, so no empty item is left behind.
    pub fn set_blur_event(&mut self, event: tilo_ui_base::component::Event) {
        self.blur_event = event;
    }

    /// Tell the reader the selection settled, as [`Self::with_selection_event`] describes it.
    pub(super) fn report_selection(&self) {
        let Some(event) = self.selection_event.clone() else {
            return;
        };
        let value = self
            .selection_range_public()
            .map(|(start, end)| {
                let (start, end) = (start.min(end), start.max(end));
                tilo_ui_base::component::Value::Text(format!("{start}:{end}"))
            })
            .unwrap_or_else(|| tilo_ui_base::component::Value::Text(String::new()));
        event.fire(Some(value));
    }

    /// [`Self::on_change`] on a field already built.
    pub fn set_on_change(&mut self, callback: impl FnMut(&str) + 'static) {
        self.on_change = Some(Box::new(callback));
        self.last_notified_text.clone_from(&self.text);
    }
}
