app t.remind "Remind" (icon: .bell, category: .office)

/// Something to call about, due on a day at a moment, assigned to a person, with how long
/// before it is due that person is reminded.
type Task {
    title: Text (title)
    done: Bool (done)
    due: Option<Date> = .none
    at: Option<Time> = .none
    who: Option<Principal> = .none
    remind: Option<std.Reminder> (reminds: due, at, who: who) = .none
}

/// The workspace's tasks.
type Board {
    tasks: Collection<Task>
}

root Board

default view board for Board

default view task for Task

/// Puts a task on the board, due for one person.
primary action new_task(
    b: Board,
    title: Text,
    due: Date,
    at: Option<Time>,
    who: Option<Principal>,
    remind: std.Reminder,
) -> Task (label: "New task") {
    b.tasks.insert(Task(title: title, due: .some(due), at: at, who: who, remind: .some(remind)))
}

/// Marks a task done; what a reminder's notification offers.
action finish(t: Task) (label: "Done") {
    t.done = true
}

/// The board: its tasks.
view board(b: Board) {
    page {
        list(b.tasks) { t -> row { text(t.title) } }
    }
}

/// A task: its title.
view task(t: Task) {
    page {
        ui.TextInput(value: t.title, placeholder: "Title")
    }
}
