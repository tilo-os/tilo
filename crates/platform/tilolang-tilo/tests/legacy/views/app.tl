app so.tilo.lang.views "Views" (icon: .eye, category: .office)

use so.tilo.lang.board as board
use so.tilo.lang.tasks as tasks
use so.tilo.lang.chat as chat
use so.tilo.lang.notes as notes

/// A view over other apps' resources: a tilolang view block, drawn live as they change, with
/// whoever wrote it as its code author, so a button in it that runs an action asks the person
/// first unless they trust its author. An agent's task answers with one; the person keeps it
/// here or discards it.
type Answer {
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

/// The views agents answered with, and those the person kept.
type Answers {
    items: Collection<Answer>
}

root answers: Answers

default view answers_list for Answers

default view answer_page for Answer

row view answer_row for Answer

/// Keeps a view an agent answered with, written by the invoker. It opens beside what the person
/// pointed at; until they keep it, it is the agent's answer.
action answer(all: Answers, title: Text, question: Text, source: Text) -> Answer {
    all.items.insert(Answer(title: title, question: question, source: source, author: invoker))
}

/// Keeps a view among the person's own.
action keep(v: Answer) (icon: .bookmark, label: "Keep") {
    require not v.kept "This view is kept already"
    v.kept = true
}

/// Throws a view away.
destructive action discard(all: Answers, v: Answer) (icon: .trash, label: "Discard") {
    all.items.remove(v)
}

/// Every view, the kept ones a place of their own, each opening in a window of its own.
view answers_list(all: Answers) {
    std.list(all, all.items, everything: "All views", empty: "No views yet")
}

/// One view in a list: its title, and what was asked.
view answer_row(v: Answer) {
    row(v.title, secondary: v.question, icon: .eye)
}

/// A view's window: the view itself, live, under its title and its actions.
view answer_page(v: Answer) {
    std.page(v, canvas: true) {
        embed(v.source, author: v.author, invocation: .none)
    }
}

/// An agent's answer to "what's left for the release?", over the workspace's tasks.
scenario "a release answer" -> Answer {
    answers.items.insert(Answer(title: "Left for the release", question: "What's left for the release?", source: "card(tasks.tasks, count: tasks.tasks.items.count(t -> not t.done)) \{\n    list(tasks.tasks.items.filter(t -> not t.done)) \{ t ->\n        row(t)\n    \}\n\}", author: invoker))
}
