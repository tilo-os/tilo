//! What a field tells whoever holds it: a submitting Enter, a committed edit, a settled
//! selection, and Escape letting go of what was typed.

use super::{CHANGE_TEXT, SUBMIT_TEXT, TextInput};
use crate::widget::{ActionEmitter, Widget as _};

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
        self.submit_action = Some(ActionEmitter::new_replace_one(binding));
        self
    }

    /// Report the selection each time it settles — a drag let go, a double or triple click, a
    /// touch release, a select all — as `<start>:<end>` byte offsets into the text, or nothing
    /// when the selection collapsed. A read-only field reports it so what reads it can act on
    /// the run someone chose (a message answered on just those words); a field being typed in
    /// keeps its selection private unless it is given one too.
    #[must_use]
    pub fn with_selection_event(mut self, event: crate::component::Event) -> Self {
        self.selection_event = Some(event);
        self
    }

    /// Tell `event` when Escape lets go of what is being typed: the field clears first.
    pub fn set_cancel_event(&mut self, event: crate::component::Event) {
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

    pub fn set_selection_event(&mut self, event: crate::component::Event) {
        self.selection_event = Some(event);
    }

    /// Tell `event` when the field's text goes from empty to non-empty, carrying what it holds
    /// now: the first change, which makes the item a list's entry names.
    pub fn set_begin_event(&mut self, event: crate::component::Event) {
        self.begin_event = event;
    }

    /// Tell `event` when the field loses the keyboard: an entry left empty is taken away by what
    /// listens, so no empty item is left behind.
    pub fn set_blur_event(&mut self, event: crate::component::Event) {
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
                crate::component::Value::Text(format!("{start}:{end}"))
            })
            .unwrap_or_else(|| crate::component::Value::Text(String::new()));
        event.fire(Some(value));
    }

    /// Run `callback` with the text on a submitting Enter, the field as control
    /// `tilo.control:<id>`, publishing the submit as its view action `submit`: a person's Enter, a
    /// screen reader, a script and an agent submit through the same action.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "the generated payload is serializable and the action is the resource's own"
    )]
    pub fn on_submit_view(mut self, callback: impl FnMut(&str) + 'static) -> Self {
        use crate::tilo_actions::{ActionBinding, ActionSpec, OperationKind, ResourceSpec};
        let id = self.test_id().unwrap_or("text_input").to_owned();
        let spec = ActionSpec::operation(
            "submit",
            "Submit",
            OperationKind::View,
            SUBMIT_TEXT,
            &serde_json::json!({ "control": id }),
        )
        .expect("the submit payload is serializable")
        .for_audience(crate::tilo_actions::Audience::Agents);
        let resource = ResourceSpec::new(format!("tilo.control:{id}"), "tilo.control", "Submit")
            .with_action(spec);
        let binding = ActionBinding::unit_for(&resource, "submit")
            .expect("the submit action is the resource's own and takes no arguments");
        self.enter_submits = true;
        self.on_submit = Some(Box::new(callback));
        self.submit_view = Some((ActionEmitter::new_replace_one(binding), resource));
        self
    }

    /// Run `callback` with the text of every committed edit, the field, as
    /// control `tilo.control:<id>`, publishing the edit as its view action
    /// `change`: typing, pasting, an IME commit, a screen reader's edit, a
    /// script's and an agent's all arrive through the same action. The
    /// control's latest text is what it reports, so a burst of edits the host
    /// has not collected yet reports once, with the text it now holds.
    #[must_use]
    pub fn on_change_view(mut self, callback: impl FnMut(&str) + 'static) -> Self {
        self.set_on_change_view(callback);
        self
    }

    /// [`Self::on_change_view`] on a field already built.
    pub fn set_on_change_view(&mut self, callback: impl FnMut(&str) + 'static) {
        let id = self.test_id().unwrap_or("text_input").to_owned();
        self.change_verb = Some(crate::widget::control_verb::ControlVerb::new(
            &id,
            "Change",
            "change",
            CHANGE_TEXT,
            crate::widget::control_verb::VerbArgument::Text,
        ));
        self.on_change = Some(Box::new(callback));
        self.last_notified_text.clone_from(&self.text);
    }
}
