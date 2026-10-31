app so.tilo.frames "Frames" (icon: .app_window, category: .system)

/// Window frames, their tabs and their stacks: what the shell draws around a place
/// (shell.md §4).
///
/// A place is one resource in a frame; a frame is a column of tabs over the place in front; a
/// stack is frames in one column with one expanded; a strip is the columns in front in one
/// workspace. The arrangement — which of them exist and where — is the person's own record,
/// synced across their devices that use this shell, with history and undo
/// (workspace-model.md §9).
///
/// Everything a frame says comes from the resource and its type: the title, the glyph, the
/// faces of who is there, and the commands its actions offer. An app draws none of it.
/// The kind of thing a frame shows in this package's own scenarios: the fields a frame draws
/// of a place, and an action its head offers.
type Document {
    title: Text (title)
    body: Text (body)
    done: Bool = false
}

/// The documents this package's scenarios write before they place them, as a root of its own:
/// a place holds a resource the store has, so a scenario makes its documents here first, then
/// opens them in frames. In a real desktop the resources are the apps' (one of the notes app's
/// notes, one of a chat's conversations); the frame draws them exactly the same way, because
/// everything it draws comes from the resource and its type.
type Notebook {
    documents: Collection<Document>
}

default view document for Document

/// A document's window: its title, and its text at the reading measure.
view document(d: Document) {
    ui.Scroll {
        page {
            stack(gap: .section) {
                ui.Text(d.title, role: .title)
                ui.Text(d.body)
            }
        }
    }
}

/// Marks a document done, or opens it again: what a frame's head offers for it.
action finish(d: Document, done: Bool = not d.done) (label: if d.done { "Reopen" } else { "Finish" }, icon: .check, on: d.done) {
    d.done = done
}
