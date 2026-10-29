app so.tilo.lang.signed "Signed" (icon: .file_text, category: .office)

/// A page: its title, and who changed it last.
type Page {
    title: Text (title)
}

root page: Page

default view page_view for Page

/// The page: its title to edit, and who edited it last, by name.
view page_view(p: Page) {
    stack {
        ui.TextInput(value: p.title, placeholder: "Title")
        if let u = updated(p) {
            text("Edited by {u.who}")
        }
    }
}
