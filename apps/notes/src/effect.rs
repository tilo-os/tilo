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
    #[effect(view, returns, kind = "read", audience = "agents")]
    InspectEditor,
    /// Find note
    ///
    /// Find the notes whose title holds some text, exact matches first.
    #[effect(view, returns, kind = "read", audience = "agents")]
    FindPage {
        #[param("Text of the title, ignoring case")]
        title: String,
    },
    /// Find text
    ///
    /// Find the blocks whose text holds some text, in one note or in every
    /// note, and answer the block ids to edit them by.
    #[effect(view, returns, kind = "read", audience = "agents")]
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
    #[effect(attended, job, cancellable)]
    ImportMarkdownFile,
    /// Import Markdown content
    ///
    /// Add Markdown text to the notebook as a note, with no file to choose.
    #[effect(returns, kind = "undoable", audience = "agents")]
    ImportMarkdownContent {
        #[param("Note title")]
        title: String,
        #[param("Markdown content")]
        body: String,
    },
    /// Export Markdown
    ///
    /// Save a note as a Markdown file, its images inside it.
    #[effect(attended, job, cancellable, row, item = pages)]
    ExportMarkdown {
        #[param("The note")]
        page: Uuid,
    },
    /// Export Markdown content
    ///
    /// Keep a note, or the whole notebook, as a Markdown file in the room's
    /// content, its images inside it, and answer its content address. No
    /// file is chosen or written: an agent hands the content on (a mail
    /// attachment, a shared file) instead of reaching this device's disk.
    #[effect(kind = "read", returns, idempotent, audience = "agents")]
    ExportMarkdownContent {
        #[param("The note; the whole notebook when neither it nor a title is given")]
        page: Option<Uuid>,
        #[param("The note's title, ignoring case, in place of its id")]
        title: Option<String>,
    },
    /// Insert image
    ///
    /// Choose an image and add it to a note.
    #[effect(attended, job, cancellable, row, item = pages)]
    InsertImage {
        #[param("The note")]
        page: Uuid,
    },
    /// Copy note link
    ///
    /// Copy a link to a note that pastes into another note.
    #[effect(row, item = pages)]
    CopyLink {
        #[param("The note")]
        page: Uuid,
    },
    /// Open link
    ///
    /// Open a web or mail link in the person's browser.
    #[effect(job)]
    OpenLink {
        #[param("The address, http(s) or mailto")]
        url: String,
    },
    /// Open resource
    ///
    /// Open something another app keeps — a file, an event, a message — by
    /// the address a link in a note gives it, and show it where it lives.
    #[effect(view)]
    OpenResource {
        #[param("The resource's link address, as tilo://open#…")]
        href: String,
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
    #[effect(view, idempotent, row, item = pages, audience = "agents")]
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
