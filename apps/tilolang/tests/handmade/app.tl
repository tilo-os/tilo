app so.tilo.lang.handmade "Shelf" (icon: .bookmark, category: .office)

/// A book on a shelf, and whether it was read.
type Book {
    title: Text (title)
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

/// Takes a book off the shelf.
destructive action remove(s: Shelf, b: Book) (icon: .trash) {
    s.books.remove(b)
}

/// Marks a book read or unread.
action mark(b: Book, read: Bool = not b.read) (label: if read { "Mark read" } else { "Mark unread" }, icon: .check, on: b.read) {
    b.read = read
}

/// The shelf's window, composed from the toolkit's components and the platform's answers
/// rather than `std.list`: the header holds the shelf's primary action, each book is a row
/// whose actions are its context menu and its swipe, and a book opens beside the list.
view shelf(s: Shelf) {
    let offered = actions(s)
    ui.NavShell(content_kind: .document, overview_title: meta(s).title, overview_count: s.books.count(b -> true)) {
        overview_actions: {
            ui.SearchButton()
            for a in offered {
                if a.primary {
                    ui.Button(a.label, icon: a.icon, role: .primary) {
                        on press: a.run
                    }
                }
            }
        }
        overview: {
            ui.VirtualList(selection_follows_focus: true, in_column: true) {
                on select(b: Book): open(b, front: false)
                on activate(b: Book): open(b)
                for b in s.books {
                    let mine = actions(b)
                    ui.ListItem(item: b) {
                        menu: {
                            for a in mine {
                                ui.MenuItem(a.label, icon: a.icon, destructive: a.destructive) {
                                    on choose: a.run
                                }
                            }
                        }
                        swipe: {
                            match mine.find(a -> a.destructive) {
                                .some(a) => ui.SwipeAction(a.label, icon: a.icon, destructive: true) {
                                    on swipe: a.run
                                },
                                .none => {}
                            }
                        }
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

/// A book's window: its title, and its actions in the header.
view book(b: Book) {
    ui.NavShell(content_kind: .document, title: meta(b).title) {
        actions: {
            for a in actions(b) {
                ui.Button(a.label, icon: a.icon, icon_only: true, toggled: a.on == .some(true)) {
                    on press: a.run
                }
            }
        }
        content: {
            ui.Scroll {
                page {
                    stack(gap: .section) {
                        ui.TextInput(value: b.title, placeholder: "Title")
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
