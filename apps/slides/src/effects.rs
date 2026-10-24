//! What Slides does that changes no deck by itself: what the editor shows,
//! presenting, opening and saving presentation files, and reading the deck
//! out as text. A file opened replaces the deck through the deck's own
//! action; everything else leaves the document alone.

use ui::prelude::Effect;
// The derive names the effect traits through this path.
use ui::tilo_resource;

/// The editor's effects, answered by [`crate::SlidesApp`].
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "slides.effect")]
pub enum SlidesEffect {
    /// Show editor
    #[effect(view)]
    Editor,
    /// Show slide overview
    #[effect(view)]
    Overview,
    /// Start presentation
    #[effect(view)]
    Present,
    /// Present on largest external display
    ///
    /// Each collaborator keeps control of their own displays.
    #[effect(view)]
    PresentLargestExternal,
    /// Present on primary external display
    ///
    /// Each collaborator keeps control of their own displays.
    #[effect(view)]
    PresentPrimaryExternal,
    /// End presentation
    #[effect(view)]
    EndPresentation,
    /// Show slide list
    ///
    /// Show or hide the list of slides beside the slide being edited;
    /// showing it returns to the editor.
    #[effect(view)]
    ShowSlideList {
        #[param("Whether the list of slides shows")]
        shown: bool,
    },
    /// Show properties
    ///
    /// Show or hide the properties of the slide or the selected element;
    /// showing them returns to the editor.
    #[effect(view)]
    ShowProperties {
        #[param("Whether the properties show")]
        shown: bool,
    },
    /// Open slide
    #[effect(view)]
    OpenSlide {
        #[param("Id of the slide")]
        slide: String,
    },
    /// Reveal element
    #[effect(view)]
    OpenElement {
        #[param("Id of the slide")]
        slide: String,
        #[param("Id of the element")]
        element: String,
    },
    /// Edit text
    ///
    /// Show the text box with its words selected for editing.
    #[effect(view)]
    EditText {
        #[param("Id of the slide")]
        slide: String,
        #[param("Id of the text element")]
        element: String,
    },
    /// Open speaker notes
    #[effect(view)]
    OpenNotes {
        #[param("Id of the slide")]
        slide: String,
    },
    /// Insert picture
    ///
    /// Choose a picture with the system picker and place it on the current
    /// slide.
    #[effect(attended, job, cancellable)]
    InsertPicture,
    /// Open presentation
    ///
    /// Replace the deck with an ODP or PPTX file: the one at `path`, or one
    /// chosen with the system picker.
    #[effect(job, cancellable, destructive)]
    OpenPresentation {
        #[param("Absolute path of the presentation; omit to use the system picker")]
        path: Option<String>,
    },
    /// Save presentation
    ///
    /// Write the deck as ODP (or PPTX, by the file's extension) to `path`, or
    /// where the person chooses with the system picker.
    #[effect(job, cancellable)]
    SaveAs {
        #[param("Absolute path to write; omit to use the system picker")]
        path: Option<String>,
    },
    /// Read deck as text
    ///
    /// The deck as text: each slide's number, layout and {#id}, its title
    /// (# line), body and other elements each ending in its {#id}, then its
    /// speaker notes.
    #[effect(returns, kind = "read")]
    ReadDeck,
    /// Import presentation from content
    ///
    /// Open a `.pptx` the room holds as content (an attachment, a shared
    /// file, an export) into this deck while it is new and untouched; a deck
    /// someone wrote is never replaced. Answers the imported outline.
    #[effect(returns, kind = "undoable", audience = "agents")]
    ImportPptxContent {
        #[param(content, "The deck's content address, `tilo.content:<hash>`")]
        content: String,
    },
    /// Export presentation as content
    ///
    /// Keep the deck as a `.pptx` in the content store, with no file to
    /// write: the caller reads, attaches or sends it by the content address
    /// it answers.
    #[effect(content, kind = "read", idempotent)]
    ExportPptxContent,
}
