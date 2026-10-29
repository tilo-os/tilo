app so.tilo.lang.today "Today" (icon: .sun, category: .office)

use so.tilo.lang.notes as notes
use so.tilo.lang.todo as todo
use so.tilo.lang.chat as chat

/// The day at a glance, over other apps: it holds references to their root resources and draws
/// their views, so each part keeps its own app's authority.
type Today {
    notebook: Option<notes.Notebook>
    todos: Option<todo.TodoList>
    chats: Option<chat.Chat>
}

root Today

default view today for Today

/// Points Today at the resources it shows.
action attach(b: Today, nb: notes.Notebook, list: todo.TodoList, c: chat.Chat) {
    b.notebook = .some(nb)
    b.todos = .some(list)
    b.chats = .some(c)
}

/// Clears the finished todos and files a note saying how many went. The changes belong to the
/// todo and notes apps, so each one asks until Today is trusted.
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

/// Today's window: a card per app it points at, holding that app's resources as rows that
/// open them: the open todos, the pinned notes, the conversations.
view today(b: Today) {
    std.page(b, canvas: true, empty: b.todos == .none and b.notebook == .none and b.chats == .none) {
        header: {
            match b.todos {
                .some(todos) => match b.notebook {
                    .some(nb) => button(morning_cleanup(todos, nb)) as morning,
                    .none => {}
                },
                .none => {}
            }
        }
        grid(min: 22rem) {
            match b.todos {
                .some(todos) => card(todos, count: todos.items.count(item -> not item.done)) {
                    list(todos.items.filter(item -> not item.done)) { item ->
                        row(item)
                    }
                },
                .none => {}
            }
            match b.notebook {
                .some(nb) => card(nb, count: nb.notes.count(n -> n.pinned)) {
                    list(nb.notes.filter(n -> n.pinned)) { n ->
                        row(n)
                    }
                },
                .none => {}
            }
            match b.chats {
                .some(c) => card(c, count: c.conversations.count(x -> true)) {
                    list(c.conversations) { x ->
                        row(x)
                    }
                },
                .none => {}
            }
        }
    }
}

/// This morning's list, notebook and chats. Each app's own first scenario fills what Today
/// attaches.
scenario "this morning" {
    let b = root Today
    b.todos = .some(root todo.TodoList)
    b.notebook = .some(root notes.Notebook)
    b.chats = .some(root chat.Chat)
}
