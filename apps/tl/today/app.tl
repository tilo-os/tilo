app so.tilo.lang.today "Today" (icon: .sun, category: .office)

use so.tilo.lang.notes as notes
use so.tilo.lang.tasks as todo
use so.tilo.lang.chat as chat

/// The day at a glance, over other apps' roots: each part is a place of its own app, drawn by
/// that app.
type Today {
}

root Today

default view today for Today

/// Clears the finished tasks, when there are any, and files a note saying how many were done.
/// Both changes are the Tasks and Notes apps' own, asked at their doors: what they did comes
/// back as their records' next readings.
action morning_cleanup(list: todo.Tasks, nb: notes.Notebook) -> Int {
    let n = list.items.count(x -> x.done)
    // The clear is refused while nothing is done, so it is asked only then.
    if n > 0 { todo.clear_done(list) }
    notes.file_note(nb, std.say("Morning: {n} done"), "morning")
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
