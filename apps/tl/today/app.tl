app so.tilo.lang.today "Today" (icon: .sun, category: .office)

use so.tilo.lang.notes as notes
use so.tilo.lang.tasks as todo
use so.tilo.lang.chat as chat

/// The day at a glance, over other apps' roots: it draws their resources with their views, so
/// each part keeps its own app's authority.
type Today {
}

root Today

default view today for Today

/// Clears the finished todos, when there are any, and files a note saying how many went. The
/// changes belong to the todo and notes apps, so each one asks until Today is trusted.
flow morning_cleanup(list: todo.Tasks, nb: notes.Notes) -> Int {
    let n = 0
    if list.items.any(x -> finished(list.items, x)) {
        n = todo.clear_done(list)
    }
    let note = notes.new_note(nb)
    notes.retitle(note, "Morning: {n} done")
    notes.tag(note, "morning")
    n
}

/// Whether `top` is `x` or one of the tasks over it: the chain of parents, followed 64 deep —
/// deeper than anyone nests a task, so a chain that loops still ends.
fn under(items: List<todo.Task>, x: todo.Task, top: todo.Task) -> Bool {
    let cur: Option<todo.Task> = .some(x)
    let found = false
    for n in items.take(64) {
        if let at = cur {
            if at == top {
                found = true
            }
            cur = at.parent
        }
    }
    found
}

/// Whether every task under `t` is done too: a done task whose steps are unfinished is not
/// finished. The tasks app reads the same rule for the `clear_done` it offers, which a flow has
/// to know before it asks anyone.
fn finished(items: List<todo.Task>, t: todo.Task) -> Bool {
    t.done and not items.any(x -> x != t and under(items, x, t) and not x.done)
}

/// Today's window: a card per app, holding that app's resources as rows that open them: the
/// open todos, the pinned notes, the chat's channels.
/// TODO(language): `list` over a collection of a union, a row per channel or category.
view today(t: Today) {
    let todos = todo.tasks
    let pinned = notes.notes.filter(.pinned)
    let talks = chat.channels
    std.page(t, canvas: true) {
        header: {
            button(morning_cleanup(todos, notes.notes)) as morning
        }
        grid(min: 22rem) {
            ui.Card(item: todos, title: meta(todos).title, count: todos.items.count(item -> not item.done)) {
                list(todos.items.filter(item -> not item.done)) { item ->
                    row(item)
                }
            }
            ui.Card(item: notes.notes, title: meta(notes.notes).title, count: pinned.len()) {
                list(pinned) { n ->
                    row(n)
                }
            }
            ui.Card(item: talks, title: meta(talks).title, count: talks.len()) {
                stack(gap: .related) {
                    for x in talks {
                        match x {
                            c: chat.Category => text(c.name, role: .caption),
                            t: chat.TextChannel => button("# {t.name}", on: open(t)),
                            // A direct conversation is a person, not a room: its place is
                            // the person's name.
                            d: chat.Direct => button(d.name, on: open(d)),
                        }
                    }
                }
            }
        }
    }
}

/// This morning's list, notebook and channels, as each app's own first scenario fills them.
scenario "this morning" {
}
