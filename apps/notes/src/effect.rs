//! What Notes does beyond its notebook: moving between notes, files in and
//! out, links and the clipboard. A control, the palette, an agent and a
//! script all reach [`NotesApp`](crate::NotesApp)'s handler with one of these.

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
    #[effect(view, returns, kind = "read")]
    InspectEditor,
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
    ImportMarkdownFile,
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
    #[effect(job, cancellable, device_only, row, item = pages)]
    ExportMarkdown {
        #[param("The note")]
        page: Uuid,
    },
    /// Insert image
    ///
    /// Choose an image and add it to a note.
    #[effect(job, cancellable, device_only, row, item = pages)]
    InsertImage {
        #[param("The note")]
        page: Uuid,
    },
    /// Copy note link
    ///
    /// Copy a link to a note that pastes into another note.
    #[effect(device_only, row, item = pages)]
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
    #[effect(view, idempotent, item = pages)]
    OpenNote {
        #[param("The note")]
        page: Uuid,
        #[param("The block to reveal")]
        block: Option<String>,
    },
    /// Reveal block range
    ///
    /// Select an exact range in a note block without changing its text.
    #[effect(view, idempotent, row, item = pages)]
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
    #[effect(view, idempotent, item = tags)]
    OpenTag {
        #[param("The tag")]
        tag: Uuid,
    },
}
