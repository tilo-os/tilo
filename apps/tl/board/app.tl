app so.tilo.board "Board"

use so.tilo.notes as notes
use so.tilo.todo as todo
use so.tilo.chat as chat

/// A dashboard over other apps: it holds references to their root resources and draws their
/// views, so each part keeps its own app's authority.
type Board {
    notebook: Option<notes.Notebook>
    todos: Option<todo.TodoList>
    chats: Option<chat.Chat>
}

/// Points the board at the resources it shows.
action attach(b: Board, nb: notes.Notebook, list: todo.TodoList, c: chat.Chat) {
    b.notebook = nb
    b.todos = list
    b.chats = c
}

/// Clears the finished todos and files a note saying how many went. The changes belong to the
/// todo and notes apps, so each one asks until the board is trusted.
flow morning_cleanup(list: todo.TodoList, nb: notes.Notebook) -> Int {
    let n = 0
    for item in list.items {
        if item.done {
            do todo.remove(list, item)
            n += 1
        }
    }
    let note = notes.new_note(nb)
    do notes.retitle(note, "Morning: {n} done")
    do notes.tag(note, "morning")
    return n
}

view main(b: Board) {
    page(full) {
        stack(gap: section) {
            row(distribute: between) {
                heading("Today")
                if b.todos.is_some() and b.notebook.is_some() {
                    button("Morning cleanup", on: morning_cleanup(b.todos.unwrap(), b.notebook.unwrap())) #morning
                }
            }
            if space.compact {
                stack(gap: section) {
                    if b.todos.is_some() {
                        todo.main(b.todos.unwrap())
                    }
                    if b.notebook.is_some() {
                        notes.note_list(b.notebook.unwrap())
                    }
                    if b.chats.is_some() {
                        chat.conversation_list(b.chats.unwrap())
                    }
                }
            } else {
                grid(min: 22rem) {
                    if b.todos.is_some() {
                        todo.main(b.todos.unwrap(), span: 2)
                    }
                    if b.notebook.is_some() {
                        notes.note_list(b.notebook.unwrap())
                    }
                    if b.chats.is_some() {
                        chat.conversation_list(b.chats.unwrap())
                    }
                }
            }
        }
    }
}
