app so.tilo.lang.views "Views" (icon: .eye, category: .office)

use so.tilo.lang.board as board
use so.tilo.lang.todo as todo
use so.tilo.lang.chat as chat
use so.tilo.lang.notes as notes

/// A view over other apps' resources: a tilolang view block, drawn live as they change, with
/// whoever wrote it as its code author, so a button in it that runs an action asks the person
/// first unless they trust its author. An agent's task answers with one; the person keeps it
/// here or discards it.
type View {
    title: Text (title)
    /// What was asked, when an agent answered with it.
    question: Text
    /// The view block, as its author wrote it.
    source: Text
    /// Who wrote the block: the agent task that answered with it, or a person. The host records
    /// it, so it cannot be claimed.
    author: Principal
    /// Whether the person kept it among their views.
    kept: Bool = false

    search {
        filter kept (icon: .bookmark)
    }
}

/// The person's views.
type Views {
    items: Collection<View>
}

root views: Views

default view views_list for Views

default view view_page for View

row view view_row for View

/// Keeps a view an agent answered with, written by the invoker. It opens beside what the person
/// pointed at; until they keep it, it is the agent's answer.
action answer(all: Views, title: Text, question: Text, source: Text) -> View {
    all.items.insert(View(title: title, question: question, source: source, author: invoker))
}

/// Keeps a view among the person's own.
action keep(v: View) (icon: .bookmark, label: "Keep") {
    require not v.kept "This view is kept already"
    v.kept = true
}

/// Throws a view away.
destructive action discard(all: Views, v: View) (icon: .trash, label: "Discard") {
    all.items.remove(v)
}

/// The views the person kept, each opening in a window of its own.
view views_list(all: Views) {
    std.list(all, all.items, place: "is:kept", everything: "All views", empty: "No views yet")
}

/// One view in a list: its title, and what was asked.
view view_row(v: View) {
    row(v.title, secondary: v.question, icon: .eye)
}

/// A view's window: the view itself, live, under its title and its actions.
view view_page(v: View) {
    std.page(v) {
        embed(v.source, author: v.author, invocation: .none)
    }
}

/// An answer to "what's left for the release?", over the release board, its todo list and its
/// chat.
scenario "a release answer" -> View {
    let b = root board.Board
    views.items.insert(View(
        title: "Left for the release",
        question: "What's left for the release?",
        source: "column(gap: .section) {\n    section(\"On the board\") {\n        for c in board.cards_of(root board.Board, (root board.Board).columns.first()!) {\n            row(c)\n        }\n    }\n}",
        author: invoker
    ))
}
