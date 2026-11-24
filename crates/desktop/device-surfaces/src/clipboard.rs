//! The clipboard history as the device publishes it.

use tilo_resource::Effect;

use crate::devices::ClipboardCopy;

/// What the clipboard resource offers: pasting a copy again, what the person does with the history,
/// and pasting the copied files into a folder.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.clipboard")]
pub enum ClipboardAction {
    /// Put a copy back on the clipboard and paste it into the app in front
    #[effect(kind = "outside")]
    #[effect(own_device)]
    Paste {
        /// The copy's key, as the history's rows carry it
        #[param("The copy")]
        copy: String,
    },
    /// Keep a copy at the front of the history, or let it go with the rest
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    Pin {
        /// The copy's key, as the history's rows carry it
        #[param("The copy")]
        copy: String,
        /// Whether to keep it
        #[param("Whether to keep it")]
        pinned: bool,
    },
    /// Remove one copy from the history
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    Remove {
        /// The copy's key, as the history's rows carry it
        #[param("The copy")]
        copy: String,
    },
    /// Clear the history, keeping what the person pinned
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    Clear,
    /// Put text on the clipboard, as if it had been copied
    #[effect(kind = "outside")]
    #[effect(own_device)]
    Copy {
        /// The text to copy
        #[param("The text to copy")]
        text: String,
    },
    /// Paste the copied files into a folder, as a copy, or a move after a cut
    #[effect(kind = "outside")]
    #[effect(own_device)]
    PasteFiles {
        /// The folder to paste them into
        #[param("The folder")]
        destination: String,
        /// What to do when a name is already there: keep both (the default), skip it, or put what
        /// is there in the Trash
        #[param("What a taken name gets")]
        conflict: Option<String>,
    },
}

/// The clipboard as a view of another language reads it (`device.clipboard`): the copies the
/// history keeps, newest first.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct ClipboardView {
    pub copies: Vec<ClipboardCopy>,
}

/// The clipboard as a view of another language reads it, and the verbs it offers: pasting a copy
/// again, keeping one, dropping one and clearing the rest.
#[must_use]
pub(crate) fn surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<ClipboardView, ClipboardAction>(
        "clipboard",
        "Clipboard",
    )
}
