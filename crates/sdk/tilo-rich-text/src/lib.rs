//! # Rich text in a Tilo app
//!
//! One document model and one editor across the OS: tilo-doc's rich text
//! ([`tilo_document::RichText`]) is the resource, and the toolkit's block
//! editor is the view of it. An app that shows rich text writes one line:
//!
//! ```ignore
//! use tilo_rich_text::RichTextViews;
//!
//! struct Doc;
//! impl ResourceApp for Doc {
//!     type Resource = tilo_document::RichText;
//!     const ID: &'static str = "example.doc";
//!     const TITLE: &'static str = "Document";
//!     fn view(cx: &mut ViewCx<RichText>) -> Element {
//!         cx.rich_text()
//!     }
//! }
//! ```
//!
//! and gets, from the framework and without writing any of it: every edit as
//! a replayable [`RichTextAction`](tilo_document::RichTextAction) (the same
//! vocabulary the palette, agents and scripts use), permission checks (a
//! viewer's keystrokes never reach the document), per-author undo with
//! typing grouped a word at a time, attributed history with History mode
//! drawn by the same editor, collaboration that keeps each person's caret
//! where they were typing, and presence keyed to paragraphs.
//!
//! [`RichTextSurface`] is the surface behind that line, for an app that
//! configures the editor or places it inside its own layout.

mod block_commands;
mod blocks;
mod composer;
mod convert;
mod draft;
mod edit;
mod image_paste;
mod source;
mod surface;
mod view;

use tilo_document::{RichText, RichTextAction};
use ui::automerge::ObjId;
use ui::framework::ViewCx;
use ui::tilo_resource::{Doc, Resource};
use ui::widget::Element;

pub use blocks::{
    blocks_under, fill_clips_under, fill_under, plain_blocks, plain_clips, put_rich_text,
    rich_text_at,
};
pub use composer::{ComposerFrame, RichTextComposer};
pub use convert::{clip_block, clip_of_content, content_of, content_of_clip, document_mark};
pub use draft::Draft;
pub use source::{Hosted, RichTextField, RichTextSource};
pub use surface::{FieldOutcome, RichTextSurface, SubmitKey};
pub use view::RichTextView;

/// A block editor for short rich text: a composer, a reply, a message row.
/// Lines wrap, paragraphs sit close, and blocks have no drag handles.
#[must_use]
pub fn compact_editor() -> ui::widget::BlockEditor {
    ui::widget::BlockEditor::new()
        .with_wrap_lines(true)
        .with_compact_paragraph_spacing(true)
        .with_block_handles(false)
}

/// Rich text in a view: `cx.rich_text()`.
pub trait RichTextViews {
    /// The document, editable where the participant may edit it.
    fn rich_text(&self) -> Element;

    /// The document in `surface`, configured by the caller.
    fn rich_text_in(&self, surface: RichTextSurface) -> Element;
}

impl<T: 'static> RichTextViews for ViewCx<RichText, T> {
    fn rich_text(&self) -> Element {
        self.rich_text_in(RichTextSurface::new())
    }

    fn rich_text_in(&self, surface: RichTextSurface) -> Element {
        self.surface(surface)
    }
}

/// A rich-text field of any resource in a view:
/// `cx.rich_text_field(&child_resource_id::<Message>("body"), body_of, EditBody)`.
pub trait RichTextFieldViews<R: Resource> {
    /// The rich text `locate` finds in the resource's document, the child
    /// resource `id` of it, edited as the resource's own action
    /// `wrap(edit)`. The resource publishes the child at the same `id` with
    /// the same `wrap` ([`publish_child`](ui::tilo_resource::publish_child)).
    fn rich_text_field(
        &self,
        id: &str,
        locate: impl Fn(&Doc) -> Option<ObjId> + 'static,
        wrap: impl Fn(RichTextAction) -> R::Action + 'static,
    ) -> Element;
}

impl<R: Resource, T: 'static> RichTextFieldViews<R> for ViewCx<R, T> {
    fn rich_text_field(
        &self,
        id: &str,
        locate: impl Fn(&Doc) -> Option<ObjId> + 'static,
        wrap: impl Fn(RichTextAction) -> R::Action + 'static,
    ) -> Element {
        Box::new(RichTextSurface::field(RichTextField::new(
            self.child::<RichText>(id, wrap),
            locate,
        )))
    }
}
