app so.tilo.lang.handmade "Shelf" (icon: .bookmark, category: .office)

/// A book on a shelf, and whether it was read.
type Book {
    title: Text
    read: Bool = false
}

/// The workspace's books.
type Shelf {
    books: Collection<Book>
}

root Shelf

default view shelf for Shelf

default view book for Book

/// Puts a book on the shelf.
primary action new_book(s: Shelf) -> Book (icon: .plus) {
    s.books.insert(Book(title: "Untitled"))
}

/// Marks a book read or unread.
action mark(b: Book, read: Bool = not b.read) (
    label: if read { "Mark read" } else { "Mark unread" },
    icon: .check,
    on: b.read,
) {
    b.read = read
}

/// Takes a book off the shelf.
destructive action remove(s: Shelf, b: Book) (icon: .trash) {
    s.books.remove(b)
}

/// The shelf's window, composed from the toolkit's components and the platform's answers
/// rather than `std.list`: a book is a row that carries its own item, and a book opens beside
/// the list. The bar's buttons and each row's menu and swipe are the host's.
view shelf(s: Shelf) {
    ui.NavShell(
        content_kind: .document,
        overview_title: meta(s).title,
        overview_count: s.books.count(b -> true),
    ) {
        overview: {
            ui.VirtualList(selection_follows_focus: true, in_column: true) {
                on select(b: Book): open(b, front: false)
                on activate(b: Book): open(b)
                for b in s.books {
                    ui.ListItem(item: b) {
                        ui.ListRow(title: b.title, mark: if b.read { .check })
                    }
                }
            }
        }
        content: {
            ui.EmptyState("No book selected", icon: .bookmark)
        }
    }
}

/// A book's window: its title, and what can be done with it behind the "…" beside it.
view book(b: Book) {
    ui.NavShell(content_kind: .document, title: meta(b).title) {
        content: {
            ui.Scroll {
                page {
                    stack(gap: .section) {
                        ui.TextInput(bind: b.title, placeholder: "Title")
                    }
                }
            }
        }
    }
}

/// A shelf with two books, the first one open.
scenario "two books" -> Book {
    let s = root Shelf
    let first = s.books.insert(Book(title: "Piranesi"))
    s.books.insert(Book(title: "The Dispossessed", read: true))
    first
}
