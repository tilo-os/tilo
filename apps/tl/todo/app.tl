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

/// Adds an item to the end of the list.
action add(list: TodoList, title: Text) -> Todo {
    return list.items.insert(Todo(title: title))
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
    return list.items.count(t -> not t.done)
}

fn title_of(list: TodoList) -> Text {
    return if list.name == "" then "Todo" else list.name
}

view main(list: TodoList) {
    page {
        stack(gap: section) {
            heading("{title_of(list)} ({open_count(list)} open)")
            stack(gap: related) {
                for item in list.items {
                    item_row(list, item)
                }
            }
            row {
                input(bind: list.draft, placeholder: "New item", fill)
                button("Add", on: add_draft(list)) #add
            }
            if not space.compact {
                input(bind: list.name, placeholder: "List name")
            }
        }
    }
}

view item_row(list: TodoList, item: Todo) {
    row(distribute: between) {
        checkbox(item.title, bind: item.done)
        button("Remove", on: remove(list, item))
    }
}
