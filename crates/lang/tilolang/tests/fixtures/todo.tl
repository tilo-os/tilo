app so.tilo.todo "Todo"

type Todo {
    title: Text
    done: Bool = false
}

type TodoList {
    name: Text
    items: Collection<Todo>
    tags: Set<Text>
}

/// Adds an item to the end of the list.
action add(list: TodoList, title: Text) -> Todo {
    return list.items.insert(Todo(title: title))
}

action toggle(item: Todo, done: Bool = not item.done) {
    item.done = done
}

action remove(list: TodoList, item: Todo) {
    list.items.remove(item)
}

action move_to(list: TodoList, item: Todo, index: Int) {
    list.items.move(item, index)
}

action rename(list: TodoList, name: Text) {
    list.name = name
}

fn open_count(list: TodoList) -> Int {
    return list.items.count(t -> not t.done)
}

view todo_list(list: TodoList) {
    column {
        text("{list.name} ({open_count(list)} open)", role: .title)
        for item in list.items {
            row {
                checkbox(item.title, bind: item.done)
                button("Remove", on: remove(list, item)) as remove
            }
        }
        input(bind: list.name, placeholder: "List name")
        button("Add", on: add(list, "New"))
    }
}
