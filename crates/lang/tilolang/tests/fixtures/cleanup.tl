app so.tilo.cleanup "Cleanup"

use so.tilo.todo as todo

/// Removes every finished item and says how many went.
flow clear_done(list: todo.TodoList) -> Int {
    let n = 0
    for item in list.items {
        if item.done {
            do todo.remove(list, item)
            n += 1
        }
    }
    return n
}

view cleanup(list: todo.TodoList) {
    button("Clear finished", on: clear_done(list))
}
