app so.tilo.lang.openby "Open by" (icon: .bookmark, category: .office)

/// A book on a shelf: what a row shows, and what a press opens.
type Book {
    title: Text
    author: Text
}

/// A label a book carries, which the app declares no view for.
type Label {
    title: Text
    note: Text
}

/// The workspace's books.
type Shelf {
    books: Collection<Book>
    labels: Collection<Label>
}

root Shelf

default view shelf for Shelf

default view book for Book

/// The shelf's window: every book a row that opens it, and every label a row that opens it too,
/// though nothing declares what a label opens through.
view shelf(s: Shelf) {
    std.page(s) {
        ui.Section("Books") {
            for b in s.books {
                ui.ListRow(b.title, secondary: b.author, item: b)
            }
        }
        ui.Section("Labels") {
            for l in s.labels {
                ui.ListRow(l.title, item: l)
            }
        }
    }
}

/// A book's window: its title and who wrote it.
view book(b: Book) {
    ui.NavShell(content_kind: .document, title: b.title) {
        content: {
            std.page(b) {
                ui.TextInput(bind: b.title, placeholder: "Title")
                ui.TextInput(bind: b.author, placeholder: "Author")
            }
        }
    }
}

/// A shelf with a book and a label on it.
scenario "two books" {
    let s = root Shelf
    s.books.insert(Book(title: "Piranesi", author: "Susanna Clarke"))
    s.labels.insert(Label(title: "Favourite", note: "Dog-eared"))
}
