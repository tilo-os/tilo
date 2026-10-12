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
    /// Open slide
    #[effect(view, row)]
    OpenSlide {
        #[param("Id of the slide")]
        slide: String,
    },
    /// Reveal element
    #[effect(view, row)]
    OpenElement {
        #[param("Id of the slide")]
        slide: String,
        #[param("Id of the element")]
        element: String,
    },
    /// Edit text
    ///
    /// Show the text box with its words selected for editing.
    #[effect(view, row)]
    EditText {
        #[param("Id of the slide")]
        slide: String,
        #[param("Id of the text element")]
        element: String,
    },
    /// Open speaker notes
    #[effect(view, row)]
    OpenNotes {
        #[param("Id of the slide")]
        slide: String,
    },
    /// Insert picture
    ///
    /// Choose a picture with the system picker and place it on the current
    /// slide.
    #[effect(job, cancellable, device_only)]
    InsertPicture,
    /// Open presentation
    ///
    /// Replace the deck with an ODP or PPTX file: the one at `path`, or one
    /// chosen with the system picker.
    #[effect(job, cancellable, destructive, device_only)]
    Open {
        #[param("Absolute path of the presentation; omit to use the system picker")]
        path: Option<String>,
    },
    /// Save presentation
    ///
    /// Write the deck as ODP (or PPTX, by the file's extension) to `path`, or
    /// where the person chooses with the system picker.
    #[effect(job, cancellable, device_only)]
    SaveAs {
        #[param("Absolute path to write; omit to use the system picker")]
        path: Option<String>,
    },
    /// The deck as a `.pptx` kept in the content store, pictures included:
    /// answers its `tilo.content:<hash>` address, no file or chooser.
    #[effect(content, permission = "read", label = "Export PowerPoint as content")]
    ExportPptxContent,
    /// Export slide picture as content
    ///
    /// One slide drawn as a 1280×720 PNG kept in the content store: answers
    /// its `tilo.content:<hash>` address.
    #[effect(content, permission = "read")]
    ExportSlidePngContent {
        #[param("Slide number as `read` shows it, 1 for the first slide")]
        slide: u32,
    },
    /// Read deck as text
    ///
    /// The deck as text: each slide's number, layout and {#id}, its title
    /// (# line), body and other elements each ending in its {#id}, then its
    /// speaker notes. add-slide, set-slide-text, move-slide and the element
    /// edits return this too.
    #[effect(returns, permission = "read")]
    Read,
}
