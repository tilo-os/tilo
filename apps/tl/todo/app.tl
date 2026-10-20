app so.tilo.todo "Todo"

type Todo {
    title: Text
    done: Bool = false
}

/// A list of things to do, with the text of the item being typed.
type TodoList {
    name: Text
    items: Collection<Todo>
    tags: Set<Text>
    draft: Text
}

/// Which items a list shows.
enum Show {
    all
    open
    done
}

root TodoList

default view todo_list for TodoList

/// Adds an item to the end of the list.
action add(list: TodoList, title: Text) -> Todo {
    list.items.insert(Todo(title: title))
}

/// Adds the typed item and clears the entry.
action add_draft(list: TodoList) {
    if list.draft != "" {
        list.items.insert(Todo(title: list.draft))
        list.draft = ""
    }
}

/// Checks or unchecks an item.
action toggle(item: Todo) {
    item.done = not item.done
}

/// Removes an item.
action remove(list: TodoList, item: Todo) {
    list.items.remove(item)
}

/// Moves an item to a place in the list.
action move_to(list: TodoList, item: Todo, index: Int) {
    list.items.move(item, index)
}

/// Renames the list.
action rename(list: TodoList, name: Text) {
    list.name = name
}

fn open_count(list: TodoList) -> Int {
    list.items.count(t -> not t.done)
}

fn title_of(list: TodoList) -> Text {
    if list.name == "" { "Todo" } else { list.name }
}

/// The items a list shows.
fn shown(list: TodoList, show: Show) -> List<Todo> {
    match show {
        .all => list.items,
        .open => list.items.filter(t -> not t.done),
        .done => list.items.filter(t -> t.done),
    }
}

/// The list's window. Which items it shows is the window's: everyone looking sees the same.
view todo_list(list: TodoList) {
    window show: Show = .all
    page {
        stack(gap: .section) {
            heading("{title_of(list)} ({open_count(list)} open)")
            row(gap: .related) {
                button("All", on: set(show, .all), style: if show == .all { .primary } else { .plain }) as all
                button("Open", on: set(show, .open), style: if show == .open { .primary } else { .plain }) as open
                button("Done", on: set(show, .done), style: if show == .done { .primary } else { .plain }) as done
            }
            stack(gap: .related) {
                for item in shown(list, show) {
                    item_row(list, item)
                }
            }
            row {
                input(bind: list.draft, placeholder: "New item", fill)
                button("Add", on: add_draft(list), style: .primary) as add
            }
            if not space.compact {
                input(bind: list.name, placeholder: "List name")
            }
        }
    }
}

view item_row(list: TodoList, item: Todo) {
    row(distribute: .between) {
        checkbox(item.title, bind: item.done)
        button("Remove", on: remove(list, item))
    }
}
