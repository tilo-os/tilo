//! A note's body as the editor reads and edits it: the rich text kept in
//! the part that keeps the note.

use super::{AutoCommit, Chain, Dispatcher, NoteList, PageId, RichTextField, RichTextSource};

/// The notes' chain and the part keeping the note.
pub(super) type Kept = (Chain<NoteList>, u64);

/// A page's body: a rich-text child resource of the note, kept in the part
/// that keeps the note and edited through the notes' chain once the app is
/// hosted by the framework. Shown without one, it reads the document it is
/// given.
pub struct PageBody {
    pub(super) field: RichTextField,
    pub(super) page: Option<PageId>,
    pub(super) kept: Option<Kept>,
}

impl PageBody {
    pub(super) fn attach(&mut self) {
        if let (Some(page), Some((chain, _))) = (self.page, &self.kept) {
            self.field
                .attach(body_dispatcher(&chain.dispatcher(), page));
        }
    }
}

/// The dispatcher `page`'s body is edited through: each rich-text edit is a
/// field edit of the part keeping the note, at the body's path.
pub(super) fn body_dispatcher(
    pages: &Dispatcher<NoteList>,
    page: PageId,
) -> Dispatcher<tilo_document::RichText> {
    pages.child(&crate::actions::page_resource_id(page), move |op| {
        crate::actions::edit_body_action(page, &op)
    })
}

impl RichTextSource for PageBody {
    fn read_only(&self) -> bool {
        self.field.read_only()
    }

    fn sync(&mut self, doc: &AutoCommit) {
        let Self { field, kept, .. } = self;
        match kept {
            Some((chain, part)) => {
                if chain.read_part(*part, |doc| field.sync(doc)).is_none() {
                    tracing::warn!(part = *part, "notes: the part keeping the note is not held");
                }
            }
            None => field.sync(doc),
        }
    }

    fn revision(&self) -> Option<u64> {
        self.field.revision()
    }

    fn read_from(&mut self, doc: &AutoCommit) -> bool {
        let Self { field, kept, .. } = self;
        match kept {
            Some((chain, part)) => chain
                .read_part(*part, |doc| field.read_from(doc))
                .unwrap_or(false),
            None => field.read_from(doc),
        }
    }

    fn with_state<T>(&self, read: impl FnOnce(&tilo_document::RichTextState) -> T) -> Option<T> {
        self.field.with_state(read)
    }

    fn apply(
        &mut self,
        edits: &mut ui::widget::Edits<'_>,
        edit: &tilo_document::RichTextAction,
    ) -> Result<(), ui::tilo_actions::ActionError> {
        self.field.apply(edits, edit)
    }

    fn presence_keys(&self) -> Option<ui::rich_edit::PresenceKeyFn> {
        self.field.presence_keys()
    }
}
