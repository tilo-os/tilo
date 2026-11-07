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

/// Today's window: one card per app, each that app's own place — the tasks its Today place
/// holds, the notes Pinned holds, the conversations Chat's Unread place finds — named as the app
/// names it, with its rows and its count, and opening the app for the rest. Today holds no place
/// of its own: a place is where its app keeps it, so the word and what it finds are the app's.
view today(t: Today) {
    let todos = todo.tasks
    std.page(t, canvas: true) {
        header: {
            button(morning_cleanup(todos, notes.notebook)) as morning
        }
        grid(min: 22rem) {
            std.place(todos, todo.Task.today.label, todos.items.matching(todo.Task.today)) { item ->
                show(item, row: true)
            }
            std.place(
                notes.notebook,
                notes.Note.pinned.label,
                notes.notebook.notes.matching(notes.Note.pinned),
            ) { n ->
                show(n, row: true)
            }
            chat.unread(chat.conversations)
        }
    }
}

/// This morning's list, notebook and channels, as each app's own first scenario fills them.
scenario "this morning" {
}
