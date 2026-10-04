//! Text controls over collaborative plain text: every edit a splice action.

use tilo_resource::text::splice_between;
use tilo_resource::{Doc, PlainText};
use ui_core::automerge::ObjId;
use ui_core::widget::{AppModel, CodeEditor, Decorator, EventCtx, ModelPath, TextInput, Widget};
use ui_core::winit::event::WindowEvent;

use super::dispatch::Dispatcher;

/// A text control a [`TextControl`] drives: it shows the text object it is
/// bound to and keeps the user's edits local, for its owner to submit.
pub trait EditsText: Widget {
    /// Show the text object `obj`, reading it on every sync, and keep edits
    /// out of the document.
    fn show_text(&mut self, obj: ObjId);
    /// Stop showing a text object.
    fn hide_text(&mut self);
    /// The text after the user's edits since the last call, if any.
    fn take_edited(&mut self) -> Option<String>;
    /// Show `text`, which is not a user edit.
    fn reset_text(&mut self, text: &str);
    /// The text shown.
    fn shown_text(&self) -> &str;
}

impl EditsText for TextInput {
    fn show_text(&mut self, obj: ObjId) {
        self.bind_text(obj);
        self.set_direct_crdt_writes(false);
    }
    fn hide_text(&mut self) {
        self.clear_text_binding();
    }
    fn take_edited(&mut self) -> Option<String> {
        self.take_dirty()
    }
    fn reset_text(&mut self, text: &str) {
        self.set_text(text);
    }
    fn shown_text(&self) -> &str {
        self.value()
    }
}

impl EditsText for CodeEditor {
    fn show_text(&mut self, obj: ObjId) {
        self.bind_text(obj);
        self.set_direct_crdt_writes(false);
    }
    fn hide_text(&mut self) {
        self.clear_text_binding();
    }
    fn take_edited(&mut self) -> Option<String> {
        self.take_dirty()
    }
    fn reset_text(&mut self, text: &str) {
        self.set_text(text);
    }
    fn shown_text(&self) -> &str {
        self.value()
    }
}

/// Finds the text object a control edits.
type Locate = Box<dyn Fn(&Doc) -> Option<ObjId>>;

/// A text field or code editor over [`PlainText`] — a resource's own text,
/// or a child text of it (a SQL buffer, a report note) — whose every edit is
/// a [`TextEdit`](tilo_resource::TextEdit) splice through `dispatcher`.
///
/// So typing is permission-checked (a viewer's keystrokes change nothing and
/// the control shows the stored text again), attributed, undone a word at a
/// time ([`PlainText`]'s coalescing), and merges with a collaborator typing
/// in the same text character by character. The control reads the text
/// object `locate` finds on every sync; a collaborator's edit, an undo or a
/// restore shows as it lands. Without a dispatcher
/// ([`view_only`](Self::view_only)) it shows the text and every edit is
/// taken back.
pub struct TextControl<W: EditsText> {
    control: W,
    dispatch: Option<Dispatcher<PlainText>>,
    locate: Locate,
    /// The text object shown.
    bound: Option<ObjId>,
    /// The text as the document holds it, which the next edit is a splice of.
    stored: String,
}

impl<W: EditsText> TextControl<W> {
    /// `control` over the text `locate` finds, edited through `dispatcher`.
    pub fn new(
        control: W,
        dispatcher: Dispatcher<PlainText>,
        locate: impl Fn(&Doc) -> Option<ObjId> + 'static,
    ) -> Self {
        let mut text = Self::view_only(control, locate);
        text.attach(dispatcher);
        text
    }

    /// `control` showing the text `locate` finds, never editing it.
    pub fn view_only(control: W, locate: impl Fn(&Doc) -> Option<ObjId> + 'static) -> Self {
        Self {
            control,
            dispatch: None,
            locate: Box::new(locate),
            bound: None,
            stored: String::new(),
        }
    }

    /// Edit through `dispatcher` from now on.
    pub fn attach(&mut self, dispatcher: Dispatcher<PlainText>) {
        self.dispatch = Some(dispatcher);
    }

    /// The control.
    pub fn control(&self) -> &W {
        &self.control
    }

    /// The control, to configure it (a caret, a selection, a highlighter).
    pub fn control_mut(&mut self) -> &mut W {
        &mut self.control
    }

    /// Bind the text `locate` finds now in `doc`: for a host outside the
    /// widget tree, before it reads the control each frame.
    pub fn pull(&mut self, doc: &mut Doc) {
        self.follow(doc);
        self.control.sync(doc, &ui_core::automerge::ROOT);
        self.stored = self.control.shown_text().to_owned();
    }

    /// Submit the control's edit since the last call as one splice: for a
    /// host outside the widget tree, after it handed the control an event.
    pub fn submit(&mut self, doc: &mut Doc) {
        let Some(edited) = self.control.take_edited() else {
            return;
        };
        let Some(edit) = splice_between(&self.stored, &edited) else {
            return;
        };
        let applied = match &self.dispatch {
            Some(dispatch) => dispatch.apply_now(doc, &edit),
            None => Err(tilo_actions::ActionError::Refused(
                "this text is open to view only".into(),
            )),
        };
        match applied {
            Ok(()) => self.stored = edited,
            Err(error) => {
                tracing::debug!(%error, "text edit refused");
                let stored = self.stored.clone();
                self.control.reset_text(&stored);
            }
        }
    }

    fn follow(&mut self, doc: &Doc) {
        let located = (self.locate)(doc);
        if located == self.bound {
            return;
        }
        if let Some(obj) = &located {
            self.control.show_text(obj.clone());
        } else {
            self.control.hide_text();
            self.control.reset_text("");
        }
        self.bound = located;
    }
}

// The control's layout, drawing and input are its own; the wrapper binds it
// to the text before each sync and turns its edits into splices after.
impl<W: EditsText> Decorator for TextControl<W> {
    type Inner = W;

    fn inner(&self) -> &W {
        &self.control
    }

    fn inner_mut(&mut self) -> &mut W {
        &mut self.control
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        {
            let (doc, _) = ui_core::doc::document(state, path);
            self.follow(doc);
        }
        self.control.sync(state, path);
        self.stored = self.control.shown_text().to_owned();
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        self.control.handle_event(event, ctx);
        self.submit(ctx.state);
    }

    fn perform_action(
        &mut self,
        action: &ui_core::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let handled = self.control.perform_action(action, state, path);
        self.submit(ui_core::doc::document(state, path).0);
        handled
    }

    fn perform_action_at(
        &mut self,
        action: &ui_core::a11y::Action,
        key_path: &[u64],
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let handled = self
            .control
            .perform_action_at(action, key_path, state, path);
        self.submit(ui_core::doc::document(state, path).0);
        handled
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui_core::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let handled = self
            .control
            .perform_semantic_action(target, action, state, path);
        self.submit(ui_core::doc::document(state, path).0);
        handled
    }
}
