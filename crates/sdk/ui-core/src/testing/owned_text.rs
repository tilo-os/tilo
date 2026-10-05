//! A text editor that owns one document string, for tests that need a real
//! document edit behind a [`TextInput`].
//!
//! A `TextInput` only edits its own buffer. An owner shows the document's
//! value, writes each user edit back, and reports the heads it captured
//! before that write so the host records one undo step — exactly what an
//! application's editor does.

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ChangeHash, ObjId, ObjType, ReadDoc, Value};

use crate::widget::{AppModel, EventCtx, ModelPath, TextInput, Widget as _};

/// A [`TextInput`] whose edits its owner writes to the string under `key`.
pub(crate) struct OwnedText {
    input: TextInput,
    key: &'static str,
    committed: Option<Vec<ChangeHash>>,
}

impl OwnedText {
    /// Own the string under `key`, edited through `input`.
    pub(crate) fn new(key: &'static str, input: TextInput) -> Self {
        Self {
            input,
            key,
            committed: None,
        }
    }

    /// Write the input's pending user edit, keeping the earliest unreported
    /// heads as the boundary of the step it opens.
    fn publish(&mut self, doc: &mut AutoCommit, root: &ObjId) {
        let Some(text) = self.input.take_dirty() else {
            return;
        };
        if crate::doc::read_str_or_text(doc, root, self.key).as_deref() == Some(text.as_str()) {
            // A caret or selection change is not an edit.
            return;
        }
        let before = doc.get_heads();
        crate::doc::put_text_diffed(doc, root, self.key, &text);
        if doc.pending_ops() > 0 {
            self.committed.get_or_insert(before);
        }
        self.show(doc, root);
    }

    /// Show the document's value: bind its text object, or adopt a plain
    /// string until the first edit turns it into one.
    fn show(&mut self, doc: &AutoCommit, root: &ObjId) {
        match doc.get(root, self.key) {
            Ok(Some((Value::Object(ObjType::Text), obj))) => self.input.bind_text(obj),
            Ok(Some((Value::Scalar(value), _))) => {
                self.input.clear_text_binding();
                if let Some(text) = value.as_str()
                    && text != self.input.value()
                {
                    self.input.set_text(text);
                }
            }
            _ => self.input.clear_text_binding(),
        }
    }
}

impl crate::widget::Decorator for OwnedText {
    type Inner = TextInput;

    crate::provides!(dyn crate::undo::CommittedActions; inner = input);

    fn inner(&self) -> &TextInput {
        &self.input
    }

    fn inner_mut(&mut self) -> &mut TextInput {
        &mut self.input
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        {
            let (doc, root) = crate::doc::document_ref(state, path);
            self.show(doc, root);
        }
        self.input.sync(state, path);
    }

    fn handle_event(&mut self, event: &winit::event::WindowEvent, ctx: &mut EventCtx<'_>) {
        self.input.handle_event(event, ctx);
        self.publish(ctx.state.doc(), ctx.path);
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        let performed = self.input.perform_action(action, state, path);
        let (doc, root) = crate::doc::document_ref(state.doc(), path);
        self.publish(doc, root);
        performed
    }

    fn perform_action_at(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        let performed = self.input.perform_action_at(action, key_path, state, path);
        let (doc, root) = crate::doc::document_ref(state.doc(), path);
        self.publish(doc, root);
        performed
    }

    fn take_committed_action(&mut self) -> Option<Vec<ChangeHash>> {
        self.committed.take()
    }
}

impl crate::undo::CommittedActions for OwnedText {
    fn take_committed_action(&mut self) -> Option<Vec<ChangeHash>> {
        self.committed.take()
    }
}
