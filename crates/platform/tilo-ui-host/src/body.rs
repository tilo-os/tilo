//! A document's body: the block editor native Notes edits a note in.
//!
//! A field marked `(body)` holds a document, in either form the language gives it: `Text` holds
//! it as Markdown, so it stays readable text to every other view and to agents; `RichText` holds
//! it as its paragraphs, each with its kind and marks. The editor shows either as blocks, and
//! each edit the person makes is written back as the value now reads — Markdown for the one, the
//! document for the other — through the same edit every bound control reports (the runtime
//! splices it into the field). A change from elsewhere (a collaborator, an agent, undo) is shown
//! when the person is not typing in it.

use std::rc::Rc;

use tilo_render_protocol::patch::rich::RichText;
use ui::LocalState;
use ui::presence::{PathSegment, PresenceKey};
use ui::widget::{Block, BlockContent, BlockEditor, Decorator, PresenceKeyFn, Widget, rich_doc};

use crate::reconcile::{EventSink, UiEvent};

/// What the field a body editor edits holds.
#[derive(Clone)]
pub enum Stored {
    /// A document written as Markdown, as a `Text` field holds it.
    Markdown(LocalState<String>),
    /// A document written as its paragraphs, as a `RichText` field holds it.
    Rich(LocalState<RichText>),
}

impl Stored {
    fn of(&self) -> Held {
        match self {
            Self::Markdown(value) => Held::Markdown(value.with(Clone::clone)),
            Self::Rich(value) => Held::Rich(value.with(Clone::clone)),
        }
    }

    fn revision(&self) -> u64 {
        match self {
            Self::Markdown(value) => value.revision(),
            Self::Rich(value) => value.revision(),
        }
    }

    fn show(&self, held: Held) {
        match (self, held) {
            (Self::Markdown(value), Held::Markdown(markdown)) => value.set(markdown),
            (Self::Rich(value), Held::Rich(rich)) => value.set(rich),
            _ => {}
        }
    }
}

/// A document as one moment holds it: what the editor shows and what was last written.
#[derive(Clone, PartialEq)]
enum Held {
    Markdown(String),
    Rich(RichText),
}

/// See the [module docs](self).
pub struct BodyEditor {
    editor: BlockEditor,
    field: Stored,
    /// What the field held the last time the editor read it.
    shown: Held,
    /// The revision of the field the editor last read, so a value nobody changed costs nothing.
    seen: u64,
    sink: EventSink,
    key: String,
    hint: String,
    presence: Option<PresenceKeyFn>,
}

impl BodyEditor {
    /// The editor of the bound input `key`, over the field value `field`. Its blocks are keyed
    /// for presence under `path`, so a collaborator's caret in the same body shows here.
    #[must_use]
    pub fn new(
        key: &str,
        field: Stored,
        sink: &EventSink,
        path: Vec<PathSegment>,
        hint: &str,
    ) -> Self {
        let presence: Option<PresenceKeyFn> = (!path.is_empty()).then(|| {
            Rc::new(move |block: &str| {
                let mut path = path.clone();
                path.push(PathSegment::Field(block.to_owned()));
                PresenceKey::Widget { path }
            }) as PresenceKeyFn
        });
        let shown = field.of();
        let seen = field.revision();
        Self {
            editor: editor_of(key, &shown, hint, presence.as_ref()),
            field,
            shown,
            seen,
            sink: Rc::clone(sink),
            key: key.to_owned(),
            hint: hint.to_owned(),
            presence,
        }
    }

    /// Report what the person changed since the last look, as the value the blocks now are.
    fn take_edits(&mut self) -> bool {
        if self.editor.take_edit_ops().is_empty() {
            return false;
        }
        let now = match &self.shown {
            Held::Markdown(shown) => {
                let markdown = self.editor.to_markdown();
                if markdown == *shown {
                    return false;
                }
                Held::Markdown(markdown)
            }
            Held::Rich(shown) => {
                let blocks: Vec<BlockContent> =
                    self.editor.blocks().iter().map(Block::content).collect();
                let rich = rich_doc::rich_of_blocks(&blocks, shown);
                if rich == *shown {
                    return false;
                }
                Held::Rich(rich)
            }
        };
        self.field.show(now.clone());
        match &now {
            Held::Markdown(markdown) => {
                self.sink
                    .push(UiEvent::Edit(self.key.clone(), markdown.clone()));
            }
            Held::Rich(rich) => {
                self.sink
                    .push(UiEvent::EditRich(self.key.clone(), rich.clone()));
            }
        }
        self.shown = now;
        self.seen = self.field.revision();
        true
    }

    /// Show a value the field got from elsewhere, unless the person is typing here.
    fn follow_value(&mut self) -> bool {
        let revision = self.field.revision();
        if revision == self.seen || self.editor.is_focused() {
            return false;
        }
        self.seen = revision;
        let held = self.field.of();
        if held == self.shown {
            return false;
        }
        self.editor = editor_of(&self.key, &held, &self.hint, self.presence.as_ref());
        self.shown = held;
        true
    }
}

/// The block editor of the bound input `key` showing `held`: it answers to the input's key, as
/// every bound control of the window does, so the window can give it the keyboard.
fn editor_of(key: &str, held: &Held, hint: &str, presence: Option<&PresenceKeyFn>) -> BlockEditor {
    // As native Notes edits a note: lines wrap at the column, paragraphs sit close.
    let mut editor = match held {
        Held::Markdown(markdown) => BlockEditor::from_markdown(markdown),
        Held::Rich(rich) => {
            let mut editor = BlockEditor::new();
            editor.set_blocks(
                rich_doc::blocks_as_written(rich)
                    .into_iter()
                    .map(Block::from_content)
                    .collect(),
            );
            editor
        }
    };
    editor = editor
        .with_id(key)
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

    /// The editor hangs its handle gutter outside the column, so its text starts on the
    /// column's edge, under the title, and still runs to the column's end, as native Notes
    /// lines a note's title up with its text. It is placed there, not margined there: a
    /// margin would have the layout ask the block stack for heights at a second width than
    /// the one it is applied at, and its blocks then paint taller than the rect they got.
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
            hung.inset.left = ui::taffy::LengthPercentageAuto::length(-(inset as f32));
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
        let field = Stored::Markdown(value.clone());
        let mut body = BodyEditor::new("b", field, &sink, Vec::new(), "");
        assert_eq!(body.editor.to_markdown().trim(), "First line");
        value.set("Changed by a collaborator".to_owned());
        assert!(body.follow_value());
        assert_eq!(
            body.editor.to_markdown().trim(),
            "Changed by a collaborator"
        );
        assert!(sink.is_empty(), "showing a value reports no edit");
    }

    #[test]
    fn a_rich_document_shows_as_its_blocks() {
        use tilo_render_protocol::patch::rich::{Attr, Block as WireBlock};
        let rich = RichText {
            blocks: vec![
                WireBlock {
                    kind: "heading".into(),
                    text: "Plan".into(),
                    attrs: [("level".to_owned(), Attr::Int(2))].into(),
                    ..WireBlock::default()
                },
                WireBlock {
                    kind: "list_item".into(),
                    text: "ship it".into(),
                    ..WireBlock::default()
                },
            ],
            embeds: Vec::new(),
        };
        let value = LocalState::new(rich.clone());
        let sink: EventSink = Rc::default();
        let field = Stored::Rich(value.clone());
        let mut body = BodyEditor::new("b", field, &sink, Vec::new(), "");
        let kinds: Vec<_> = body
            .editor
            .blocks()
            .iter()
            .map(|block| block.kind.clone())
            .collect();
        assert_eq!(
            kinds,
            vec![
                ui::widget::BlockKind::Heading(2),
                ui::widget::BlockKind::Bullet { indent: 0 }
            ]
        );
        assert!(sink.is_empty(), "showing a value reports no edit");

        // A document from elsewhere replaces what the editor shows.
        let mut changed = rich;
        changed.blocks[1].text = "shipped".into();
        value.set(changed);
        assert!(body.follow_value());
        assert_eq!(body.editor.blocks()[1].content().text, "shipped");
    }
}
