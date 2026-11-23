//! What a field tells its listeners when its text changes: the action it is bound to, its own
//! state, and its change verb or callback, each told once per change.

use super::TextInput;

/// One committed edit of a field: what it held before and after. A binding takes the text it
/// holds now ([`TextInput::on_action`]) or the splice between the two
/// ([`TextInput::on_text_edit`]).
pub(super) struct Edited {
    pub(super) before: String,
    pub(super) after: String,
}

impl TextInput {
    pub(super) fn notify_user_change(&mut self, physical_text: bool) {
        if !self.dirty || self.last_notified_text == self.text {
            return;
        }
        self.report_change(physical_text);
    }

    /// Tell the field's listeners what it holds now — the action it is bound to, the field's own
    /// state, and its change verb (or callback) — and record what was told.
    ///
    /// The one writer of [`Self::last_notified_text`] once a field is built: every path that
    /// reports an edit comes through here, so the text a listener hears is the text the field
    /// records as told, and a path that set the field's text itself (a programmatic seed, a
    /// document's own value) records it without telling anyone by not calling this at all.
    /// Callers decide whether what they report is a person's edit ([`Self::notify_user_change`],
    /// which asks the dirty mark) or a state the person's own action caused (an entry letting go
    /// of what it sent).
    pub(super) fn report_change(&mut self, physical_text: bool) {
        let before = std::mem::replace(&mut self.last_notified_text, self.text.clone());
        if let Some(action) = &mut self.action {
            let after = self.text.clone();
            let _ = crate::bound_dispatch::with_physical_text(physical_text, || {
                action.emit(Edited { before, after })
            });
        }
        // The field's own state stays the one a bound local edit reads: an
        // action publishing the text does not stand in for it.
        self.publish_local_state();
        if let Some(verb) = &mut self.change_verb {
            verb.emit(self.text.clone());
            return;
        }
        if let Some(callback) = self.on_change.as_mut() {
            callback(&self.text);
        }
    }

    pub(super) fn publish_local_state(&mut self) {
        if let Some(binding) = &mut self.local_state {
            binding.publish(&self.text);
        }
    }
}
