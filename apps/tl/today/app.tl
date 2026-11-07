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

/// Clears the finished todos, when there are any, and files a note saying how many went. The
/// changes belong to the todo and notes apps, so each one asks until Today is trusted.
action morning_cleanup(list: todo.Tasks, nb: notes.Notebook) -> Int {
    let n = 0
    // The clear is refused while nothing is done, so it is asked only then.
    if list.items.any(x -> x.done) {
        n = todo.clear_done(list)
    }
    let note = notes.new_note(nb)
    notes.retitle(note, "Morning: {n} done")
    notes.tag(note, "morning")
    n
}

/// Today's window: a card per app, holding that app's place as its own rows say it: the open
/// todos, the pinned notes, the conversations the chat's sidebar holds. Each card says how
/// many there are and opens the app for the rest.
view today(t: Today) {
    let todos = todo.tasks
    let pinned = notes.notebook.notes.filter(.pinned)
    let talks = chat.conversations_of(chat.conversations)
    std.page(t, canvas: true) {
        header: {
            button(morning_cleanup(todos, notes.notebook)) as morning
        }
        grid(min: 22rem) {
            std.summary(
                todos,
                meta(todos).title,
                todos.items.filter(item -> not item.done),
            ) { item ->
                show(item, row: true)
            }
            std.summary(notes.notebook, meta(notes.notebook).title, pinned) { n ->
                show(n, row: true)
            }
            std.summary(chat.conversations, meta(chat.conversations).title, talks) { x ->
                show(x, row: true)
            }
        }
    }
}

/// This morning's list, notebook and channels, as each app's own first scenario fills them.
scenario "this morning" {
}
