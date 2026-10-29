app so.tilo.lang.today "Today" (icon: .sun, category: .office)

use so.tilo.lang.notes as notes
use so.tilo.lang.tasks as todo
use so.tilo.lang.chat as chat

/// The day at a glance, over other apps' roots: it draws their resources with their views, so
/// each part keeps its own app's authority.
type Today {
}

root Today

default view today for Today

/// Clears the finished todos and files a note saying how many went. The changes belong to the
/// todo and notes apps, so each one asks until Today is trusted.
flow morning_cleanup(list: todo.Tasks, nb: notes.Notes) -> Int {
    let n = 0
    for item in list.items {
        if item.done {
            todo.remove(list, item)
            n += 1
        }
    }
    let note = notes.new_note(nb)
    notes.retitle(note, "Morning: {n} done")
    notes.tag(note, "morning")
    n
}

/// Today's window: a card per app, holding that app's resources as rows that open them: the
/// open todos, the pinned notes, the chat's channels.
/// TODO(language): `list` over a collection of a union, a row per channel or category.
view today(t: Today) {
    let todos = todo.tasks
    let pinned = notes.notes.filter(.pinned)
    let talks = chat.channels
    std.page(t, canvas: true) {
        header: {
            button(morning_cleanup(todos, notes.notes)) as morning
        }
        grid(min: 22rem) {
            card(todos, count: todos.items.count(item -> not item.done)) {
                list(todos.items.filter(item -> not item.done)) { item ->
                    row(item)
                }
            }
            card(notes.notes, count: pinned.len()) {
                list(pinned) { n ->
                    row(n)
                }
            }
            card(talks, count: talks.len()) {
                stack(gap: .related) {
                    for x in talks {
                        match x {
                            c: chat.Category => text(c.name, role: .caption),
                            t: chat.TextChannel => button("# {t.name}", on: open(t)),
                        }
                    }
                }
            }
        }
    }
}

/// This morning's list, notebook and channels, as each app's own first scenario fills them.
scenario "this morning" {
}
