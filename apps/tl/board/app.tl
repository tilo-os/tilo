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

root Board

default view board for Board

/// Points the board at the resources it shows.
action attach(b: Board, nb: notes.Notebook, list: todo.TodoList, c: chat.Chat) {
    b.notebook = .some(nb)
    b.todos = .some(list)
    b.chats = .some(c)
}

/// Clears the finished todos and files a note saying how many went. The changes belong to the
/// todo and notes apps, so each one asks until the board is trusted.
flow morning_cleanup(list: todo.TodoList, nb: notes.Notebook) -> Int {
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

/// The board's window.
view board(b: Board) {
    page(full) {
        stack(gap: .section) {
            row(distribute: .between) {
                heading("Today")
                match b.todos {
                    .some(list) => match b.notebook {
                        .some(nb) => button("Morning cleanup", on: morning_cleanup(list, nb)) as morning,
                        .none => {}
                    },
                    .none => {}
                }
            }
            if space.compact {
                stack(gap: .section) {
                    match b.todos {
                        .some(list) => todo.todo_list(list),
                        .none => {}
                    }
                    match b.notebook {
                        .some(nb) => notes.notebook(nb),
                        .none => {}
                    }
                    match b.chats {
                        .some(c) => chat.conversation_list(c),
                        .none => {}
                    }
                }
            } else {
                grid(min: 22rem) {
                    match b.todos {
                        .some(list) => todo.todo_list(list, span: 2),
                        .none => {}
                    }
                    match b.notebook {
                        .some(nb) => notes.notebook(nb),
                        .none => {}
                    }
                    match b.chats {
                        .some(c) => chat.conversation_list(c),
                        .none => {}
                    }
                }
            }
        }
    }
}
