//! A document's body: the block editor native Notes edits a note in, over a `Text` field.
//!
//! The field holds the body as Markdown, so it stays readable text to every other view and to
//! agents. The editor shows it as blocks; each edit the person makes is written back as the
//! Markdown the blocks now are, through the same edit every bound input reports (the runtime
//! splices it into the field). A change from elsewhere (a collaborator, an agent, undo) is shown
//! when the person is not typing in it.

use std::rc::Rc;

use ui::LocalState;
use ui::presence::{PathSegment, PresenceKey};
use ui::widget::{BlockEditor, Decorator, PresenceKeyFn, Widget};

use crate::reconcile::{EventSink, UiEvent};

/// See the [module docs](self).
pub struct BodyEditor {
    editor: BlockEditor,
    /// What the field holds, as the window last read it.
    value: LocalState<String>,
    /// The Markdown the editor shows.
    shown: String,
    sink: EventSink,
    key: String,
    hint: String,
    presence: Option<PresenceKeyFn>,
}

impl BodyEditor {
    /// The editor of the bound input `key`, over the field value `value`. Its blocks are keyed
    /// for presence under `path`, so a collaborator's caret in the same body shows here.
    #[must_use]
    pub fn new(
        key: &str,
        value: &LocalState<String>,
        sink: &EventSink,
        path: Vec<PathSegment>,
        hint: &str,
    ) -> Self {
        let shown = value.with(Clone::clone);
        let presence: Option<PresenceKeyFn> = (!path.is_empty()).then(|| {
            Rc::new(move |block: &str| {
                let mut path = path.clone();
                path.push(PathSegment::Field(block.to_owned()));
                PresenceKey::Widget { path }
            }) as PresenceKeyFn
        });
        Self {
            editor: editor_of(&shown, hint, presence.as_ref()),
            value: value.clone(),
            shown,
            sink: Rc::clone(sink),
            key: key.to_owned(),
            hint: hint.to_owned(),
            presence,
        }
    }

    /// Report what the person changed since the last look, as the Markdown of the blocks.
    fn take_edits(&mut self) -> bool {
        if self.editor.take_edit_ops().is_empty() {
            return false;
        }
        let markdown = self.editor.to_markdown();
        if markdown == self.shown {
            return false;
        }
        self.shown.clone_from(&markdown);
        self.value.set(markdown.clone());
        self.sink
            .borrow_mut()
            .push(UiEvent::Edit(self.key.clone(), markdown));
        true
    }

    /// Show a value the field got from elsewhere, unless the person is typing here.
    fn follow_value(&mut self) -> bool {
        let value = self.value.with(Clone::clone);
        if value == self.shown || self.editor.is_focused() {
            return false;
        }
        self.editor = editor_of(&value, &self.hint, self.presence.as_ref());
        self.shown = value;
        true
    }
}

/// The block editor showing `markdown`.
fn editor_of(markdown: &str, hint: &str, presence: Option<&PresenceKeyFn>) -> BlockEditor {
    // As native Notes edits a note: lines wrap at the column, paragraphs sit close.
    let mut editor = BlockEditor::from_markdown(markdown)
        .with_empty_hint(hint)
        .with_wrap_lines(true)
        .with_compact_paragraph_spacing(true);
    if let Some(key) = presence {
        editor.set_presence_key(Rc::clone(key));
    }
    editor
}

impl Decorator for BodyEditor {
    type Inner = BlockEditor;

    fn inner(&self) -> &BlockEditor {
        &self.editor
    }

    fn inner_mut(&mut self) -> &mut BlockEditor {
        &mut self.editor
    }

    fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        let edited = self.take_edits();
        let followed = self.follow_value();
        let inner = self.editor.update(state, path);
        edited || followed || inner
    }

    /// The editor hangs its handle gutter outside the column (a negative leading margin as
    /// wide as the gutter), so its text starts on the column's edge, under the title, and still
    /// runs to the column's end, as native Notes lines a note's title up with its text.
    fn build_layout(
        &mut self,
        taffy: &mut ui::taffy::TaffyTree<()>,
        theme: &ui::theme::Theme,
        node_map: &mut std::collections::HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        let inset = self.editor.text_inset(theme);
        let node = self.editor.build_layout(taffy, theme, node_map);
        if let Ok(style) = taffy.style(node) {
            let mut hung = style.clone();
            let margin = -(inset as f32);
            hung.margin.left = ui::taffy::LengthPercentageAuto::length(margin);
            // The node must take what the column offers plus the hung gutter.
            let _ = taffy.set_style(node, hung);
        }
        node
    }

    fn handle_event(
        &mut self,
        event: &ui::winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        self.editor.handle_event(event, ctx);
        self.take_edits();
    }

    fn name(&self) -> &'static str {
        "TilolangBody"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_from_elsewhere_shows_and_the_shown_markdown_follows_it() {
        let value = LocalState::new("First line".to_owned());
        let sink: EventSink = Rc::default();
        let mut body = BodyEditor::new("b", &value, &sink, Vec::new(), "");
        assert_eq!(body.editor.to_markdown().trim(), "First line");
        value.set("Changed by a collaborator".to_owned());
        assert!(body.follow_value());
        assert_eq!(
            body.editor.to_markdown().trim(),
            "Changed by a collaborator"
        );
        assert!(sink.borrow().is_empty(), "showing a value reports no edit");
    }
}
