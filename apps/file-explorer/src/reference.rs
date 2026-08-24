//! What a selected file is *about*.
//!
//! The file explorer's half of "select anything and say something about it".
//! It is deliberately the *same* gesture as the word processor's, arriving
//! through the same trait, so the compose surface never learns which app it is
//! talking to — that is what makes the idea general rather than a feature two
//! apps happen to share.
//!
//! ## The address is the path
//!
//! A file's durable identity on a workspace mount is where it is. The path is
//! what the FUSE layer resolves, what chat's own attachments already reference,
//! and what means the same thing on another peer's machine — so
//! `files.entry:<path>` is the address, under the `files.entry` kind tag the
//! action model already names for this app.
//!
//! Unlike a text selection there is no [`Span`](qos_reference::Span): a file is
//! referenced whole. The absence is meaningful rather than missing — it is how
//! a reader knows the remark is about the file and not a part of it.

use std::path::Path;

use qos_reference::{Referent, ResourceRef};

use crate::files_app::FileExplorerWidget;

/// Kind tag for a file or folder in the explorer.
pub const ENTRY_KIND: &str = "files.entry";

/// The address of a file, as a whole.
///
/// The label is the file's own name — what a person recognises — while the
/// path is the address. A file renamed on another peer keeps a different label
/// and the same identity in the message that referenced it, which is exactly
/// the split this crate exists to hold.
pub fn entry_reference(path: &Path) -> ResourceRef {
    let display = path.display().to_string();
    let label = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        // A path with no final component (a mount root) is labelled by itself
        // rather than by an empty chip.
        .unwrap_or_else(|| display.clone());
    ResourceRef::whole(ENTRY_KIND, format!("{ENTRY_KIND}:{display}"), label)
}

impl Referent for FileExplorerWidget {
    /// The explorer's selection lives in the widget rather than the shared
    /// document, so the document is not consulted; the parameter is part of the
    /// shared trait so that apps which *do* read their tree from the CRDT can.
    fn selection_reference(&self, _doc: &ui::automerge::AutoCommit) -> Option<ResourceRef> {
        self.primary_path().as_deref().map(entry_reference)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// The headline property, in the second app and in the same shape as the
    /// first: a selection produces an address, asserted on the id.
    #[test]
    fn a_file_selection_resolves_to_that_file() {
        let r = entry_reference(&PathBuf::from("/var/workspace/photos/harbour.png"));
        assert_eq!(
            r.resource_id().as_str(),
            "files.entry:/var/workspace/photos/harbour.png"
        );
        assert_eq!(r.kind, ENTRY_KIND);
    }

    /// A file is referenced whole, and the missing span is how a reader knows.
    #[test]
    fn a_file_is_referenced_whole() {
        let r = entry_reference(&PathBuf::from("/var/workspace/run-sheet.md"));
        assert_eq!(r.span, None);
        assert!(!r.is_partial());
    }

    /// The label is the name a person recognises; the address is the path. Two
    /// files that share a name must not share an address.
    #[test]
    fn the_name_is_the_label_and_the_path_is_the_address() {
        let a = entry_reference(&PathBuf::from("/var/workspace/a/notes.md"));
        let b = entry_reference(&PathBuf::from("/var/workspace/b/notes.md"));
        assert_eq!(a.label, "notes.md");
        assert_eq!(b.label, "notes.md", "same name");
        assert_ne!(
            a.resource_id(),
            b.resource_id(),
            "two different files must not collide on one address"
        );
    }

    #[test]
    fn a_folder_is_referenceable_too() {
        let r = entry_reference(&PathBuf::from("/var/workspace/photos"));
        assert_eq!(r.label, "photos");
        assert_eq!(r.resource_id().as_str(), "files.entry:/var/workspace/photos");
    }
}
