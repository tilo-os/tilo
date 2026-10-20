app so.tilo.todo "Todo" (icon: .list_checks)

/// Something to do, and whether it is done.
type Todo {
    title: Text (title)
    done: Bool = false
    /// Anything worth remembering about it.
    notes: Text (body)
}

/// A list of things to do, with the text of the item being typed.
type TodoList {
    name: Text (title)
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

/// Adds the typed item and clears the entry.
action add_draft(list: TodoList) {
    if list.draft != "" {
        list.items.insert(Todo(title: list.draft))
        list.draft = ""
    }
}

/// Checks or unchecks an item.
action toggle(item: Todo) (icon: .check, label: "Check or uncheck") {
    item.done = not item.done
}

/// Removes an item.
destructive action remove(list: TodoList, item: Todo) (icon: .trash) {
    list.items.remove(item)
}

/// Removes every item that is done.
destructive action clear_done(list: TodoList) -> Int (icon: .circle_check, label: "Clear done") {
    let n = 0
    for item in list.items {
        if item.done {
            list.items.remove(item)
            n += 1
        }
    }
    n
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
view todo_list(todos: TodoList) {
    window show: Show = .all
    list(shown(todos, show), filter: show, empty: "Nothing to do")
}

/// One item in a list: a checkbox for whether it is done.
view item_row(item: Todo) {
    row(item.title, checkbox: item.done)
}

/// An item's window.
view item_editor(item: Todo) {
    document {
        input(bind: item.title, placeholder: "Title")
        toggle(label: "Done", bind: item.done)
        input(bind: item.notes, placeholder: "Notes", lines: 6)
    }
}

/// A short list of what open items it has, for showing inside another app's view.
view summary(list: TodoList) {
    section("{title_of(list)} ({open_count(list)} open)") {
        stack(gap: .related) {
            for item in shown(list, .open) {
                checkbox(item.title, bind: item.done)
            }
        }
    }
}

/// A household list on a Saturday morning: enough to scroll, some of it done.
scenario "a saturday list" -> Todo {
    let list = root TodoList
    list.name = "Saturday"
    list.items.insert(Todo(title: "Take the bins out", done: true))
    list.items.insert(Todo(title: "Buy oat milk and eggs"))
    let call = list.items.insert(Todo(title: "Call the landlord about the boiler", notes: "Ask whether Tuesday morning works for the engineer."))
    list.items.insert(Todo(title: "Water the plants", done: true))
    list.items.insert(Todo(title: "Return the library books"))
    list.items.insert(Todo(title: "Book a haircut"))
    list.items.insert(Todo(title: "Pay the electricity bill", done: true))
    list.items.insert(Todo(title: "Fix the bike's back brake"))
    list.items.insert(Todo(title: "Reply to Mia about Sunday lunch"))
    list.items.insert(Todo(title: "Clean the oven"))
    list.items.insert(Todo(title: "Pick up the parcel from the post office", notes: "Bring an ID; it closes at one."))
    list.items.insert(Todo(title: "Change the bed sheets", done: true))
    list.items.insert(Todo(title: "Order a birthday present for Sam"))
    list.items.insert(Todo(title: "Back up the laptop"))
    call
}

/// A list everyone has finished.
scenario "all done" {
    let list = root TodoList
    list.name = "Packing"
    list.items.insert(Todo(title: "Passports", done: true))
    list.items.insert(Todo(title: "Chargers", done: true))
    list.items.insert(Todo(title: "Sun cream", done: true))
}
