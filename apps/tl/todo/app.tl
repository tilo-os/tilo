app so.tilo.lang.todo "Todo" (icon: .list_checks, category: .office)

/// Something to do, and whether it is done.
type Todo {
    title: Text (title)
    done: Bool = false
    /// The day it is due by, if any.
    due: Option<Date> = .none
    /// Anything worth remembering about it.
    notes: Text (body)

    search {
        filter "Open" = not done (icon: .circle)
        filter done (icon: .circle_check)
        filter due
    }
}

/// A list of things to do.
type TodoList {
    name: Text (title)
    items: Collection<Todo>
    tags: Set<Text>
}

root todos: TodoList

default view todo_list for TodoList

default view item_editor for Todo

row view item_row for Todo

/// Adds an item to the end of the list.
action add(list: TodoList, title: Text) -> Todo {
    list.items.insert(Todo(title: title))
}

/// Adds an empty item to the end of the list.
primary action new_item(list: TodoList) -> Todo (icon: .plus) {
    list.items.insert(Todo(title: ""))
}

/// Checks or unchecks an item.
action check(item: Todo, done: Bool = not item.done) (label: if done { "Check" } else { "Uncheck" }, icon: .check, on: item.done) {
    item.done = done
}

/// Removes an item.
destructive action remove(list: TodoList, item: Todo) (icon: .trash) {
    list.items.remove(item)
}

/// Removes every item that is done.
destructive action clear_done(list: TodoList) -> Int (icon: .circle_check, label: "Clear done") {
    require list.items.any(.done) "Nothing is done yet"
    list.items.remove_where(.done)
}

/// Moves an item to just after another.
action move(list: TodoList, item: Todo, after: Todo) {
    list.items.move(item, after: after)
}

/// Renames the list.
action rename(list: TodoList, name: Text) {
    list.name = name
}

/// The list's window: its items, open and done ones apart as the item's filters say.
view todo_list(todos: TodoList) {
    std.list(todos, todos.items, everything: "All", empty: "Nothing to do")
}

/// One item in a list: a checkbox for whether it is done.
view item_row(item: Todo) {
    row(item.title, checkbox: item.done, meta: item.due)
}

/// An item's window.
view item_editor(item: Todo) {
    std.document(item) {
        row(gap: .related, align: .center) {
            checkbox(bind: item.done)
            input(bind: item.title, placeholder: "Title", fill)
        }
        field("Due") {
            date(bind: item.due)
        }
        input(bind: item.notes, placeholder: "Add notes", lines: 6)
    }
}

/// A household list on a Saturday morning: enough to scroll, some of it done.
scenario "a saturday list" -> Todo {
    todos.name = "Saturday"
    todos.items.insert(Todo(title: "Take the bins out", done: true))
    todos.items.insert(Todo(title: "Buy oat milk and eggs", due: 2026-10-24))
    let call = todos.items.insert(Todo(title: "Call the landlord about the boiler", notes: "Ask whether Tuesday morning works for the engineer.", due: 2026-10-27))
    todos.items.insert(Todo(title: "Water the plants", done: true))
    todos.items.insert(Todo(title: "Return the library books"))
    todos.items.insert(Todo(title: "Book a haircut"))
    todos.items.insert(Todo(title: "Pay the electricity bill", done: true, due: 2026-10-20))
    todos.items.insert(Todo(title: "Fix the bike's back brake"))
    todos.items.insert(Todo(title: "Reply to Mia about Sunday lunch"))
    todos.items.insert(Todo(title: "Clean the oven"))
    todos.items.insert(Todo(title: "Pick up the parcel from the post office", notes: "Bring an ID; it closes at one."))
    todos.items.insert(Todo(title: "Change the bed sheets", done: true))
    todos.items.insert(Todo(title: "Order a birthday present for Sam", due: 2026-11-02))
    todos.items.insert(Todo(title: "Back up the laptop"))
    call
}

/// A list everyone has finished.
scenario "all done" {
    todos.name = "Packing"
    todos.items.insert(Todo(title: "Passports", done: true))
    todos.items.insert(Todo(title: "Chargers", done: true))
    todos.items.insert(Todo(title: "Sun cream", done: true))
}
