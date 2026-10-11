//! What Notes does beyond its notebook: moving between notes, files in and
//! out, links and the clipboard. A control, the palette, an agent and a
//! script all reach [`NotesApp`](crate::NotesApp)'s handler with one of these.

use tilo_actions::ActionSpec;
use ui::prelude::Effect;
use ui::tilo_resource;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "notes.effect")]
pub enum NotesEffect {
    /// Inspect editor
    ///
    /// Read the selected and bound page identities, content-free body hashes,
    /// editor focus and caret without selecting a note or changing its body.
    #[effect(view, returns, permission = "read")]
    InspectEditor,
    /// Find note
    ///
    /// Find the notes whose title holds some text, exact matches first.
    #[effect(view, returns, permission = "read")]
    FindPage {
        #[param("Text of the title, ignoring case")]
        title: String,
    },
    /// Find text
    ///
    /// Find the blocks whose text holds some text, in one note or in every
    /// note, and answer the block ids to edit them by.
    #[effect(view, returns, permission = "read")]
    FindText {
        #[param("The text to find, ignoring case")]
        text: String,
        #[param("The note to search (default every note)")]
        page: Option<Uuid>,
    },
    /// Back
    ///
    /// Return to the note shown before this one.
    #[effect(view)]
    Back,
    /// Forward
    ///
    /// Go to the note shown after this one.
    #[effect(view)]
    Forward,
    /// Import Markdown
    ///
    /// Choose a Markdown file and add it to the notebook as a note.
    #[effect(job, cancellable, device_only)]
    ImportMarkdown,
    /// Import Markdown content
    ///
    /// Add Markdown text to the notebook as a note, with no file to choose.
    #[effect(returns)]
    ImportMarkdownContent {
        #[param("Note title")]
        title: String,
        #[param("Markdown content")]
        body: String,
    },
    /// Export Markdown
    ///
    /// Save a note as a Markdown file, its images inside it.
    #[effect(row, job, cancellable, device_only)]
    ExportMarkdown {
        #[param("The note")]
        page: Uuid,
    },
    /// Insert image
    ///
    /// Choose an image and add it to a note.
    #[effect(row, job, cancellable, device_only)]
    InsertImage {
        #[param("The note")]
        page: Uuid,
    },
    /// Copy note link
    ///
    /// Copy a link to a note that pastes into another note.
    #[effect(row, device_only)]
    CopyLink {
        #[param("The note")]
        page: Uuid,
    },
    /// Open link
    ///
    /// Open a web or mail link in the person's browser.
    #[effect(job, device_only)]
    OpenLink {
        #[param("The address, http(s) or mailto")]
        url: String,
    },
    /// Open note
    ///
    /// Show a note, and the block in it when one is named.
    #[effect(view, row, idempotent)]
    Open {
        #[param("The note")]
        page: Uuid,
        #[param("The block to reveal")]
        block: Option<String>,
    },
    /// Reveal block range
    ///
    /// Select an exact range in a note block without changing its text.
    #[effect(view, row, idempotent)]
    RevealBlockRange {
        #[param("The note")]
        page: Uuid,
        #[param("The block to reveal")]
        block: String,
        #[param("Start offset")]
        start: usize,
        #[param("End offset")]
        end: usize,
        #[param("Offset unit", choices = ["Chars", "Bytes"])]
        unit: String,
    },
    /// Browse tagged notes
    ///
    /// Show the notes carrying a tag.
    #[effect(view, row, idempotent)]
    OpenTag {
        #[param("The tag")]
        tag: Uuid,
    },
}

impl NotesEffect {
    /// This effect as the action `id` an item offers, labelled `label`:
    /// opening a note from its row, a backlink or a tag.
    pub(crate) fn offered(&self, id: impl Into<String>, label: impl Into<String>) -> ActionSpec {
        let mut spec = self.bound();
        spec.id = id.into();
        spec.label = label.into();
        spec
    }
}
