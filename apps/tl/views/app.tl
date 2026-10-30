app so.tilo.lang.views "Views" (icon: .eye, category: .system, records: .own)

use so.tilo.lang.chat as chat

/// A view of the person's own: a tilolang `view` definition, written as an app declares one
/// (`view open_threads(m: mail.Mail) { ... }`), kept as their code in their own record. They
/// wrote it, their agent answered with it, or someone sent it to them. It names no app: every
/// app the workspace has installed is in scope by its name, checked when it is drawn, and it
/// reads as the person reads, so it shows each person what they can see. Who wrote it, as the
/// host recorded them, is its code author: what it does with an action asks the person first
/// unless they trust that author.
type WrittenView {
    title: Text (title)
    /// What was asked, when an agent answered with it.
    question: Text
    /// The view, as its author wrote it.
    definition: Text
    /// Who wrote it, as the host recorded them: the agent task that answered with it, the
    /// person, or who sent it to them. It cannot be claimed.
    author: Principal
    /// Whether the person kept it. An answer they have not kept is still the agent's answer.
    kept: Bool = false
    /// The type of the one resource it takes (`so.tilo.lang.mail.Thread`), as the check found it:
    /// what it can be the default view of. Empty for a view that stands alone.
    shows: Text

    search {
        filter kept (icon: .bookmark)
    }
}

/// A kind of device a person chooses how things show on.
enum Device {
    /// Desktop
    desktop
    /// Tablet
    tablet
    /// Phone
    phone
}

/// How the person shows a type on one kind of device: through one of their own views.
type Default {
    /// The type, as the platform names it (`so.tilo.lang.mail.Thread`).
    shows: Text
    device: Device
    chosen: WrittenView
}

/// The person's views, and the types they show through one of them.
type Views {
    items: Collection<WrittenView>
    defaults: Collection<Default>
}

root views: Views

default view views_list for Views

default view view_page for WrittenView

row view view_row for WrittenView

/// Keeps the view an agent answered with, written by the invoker, which the host records. It
/// opens beside what the person pointed at; until they keep it, it is the agent's answer.
action answer(all: Views, title: Text, question: Text, definition: Text, shows: Text = "") -> WrittenView {
    all.items.insert(WrittenView(title: title, question: question, definition: definition, author: invoker, shows: shows))
}

/// A view the person writes, kept among their own.
action write(all: Views, title: Text, definition: Text, shows: Text = "") -> WrittenView (icon: .plus, label: "New view") {
    all.items.insert(WrittenView(title: title, definition: definition, author: invoker, kept: true, shows: shows))
}

/// Keeps a view among the person's own.
action keep(v: WrittenView) (icon: .bookmark, label: "Keep") {
    require not v.kept "This view is kept already"
    v.kept = true
}

/// Throws a view away, and with it how a type showed through it.
destructive action discard(all: Views, v: WrittenView) (icon: .trash, label: "Discard") {
    for d in all.defaults.filter(d -> d.chosen == v) {
        all.defaults.remove(d)
    }
    all.items.remove(v)
}

/// Makes a view how the person shows its type on a kind of device.
action make_default(all: Views, v: WrittenView, device: Device = .desktop) (icon: .star, label: "Show this type with it") {
    require v.shows != "" "This view shows no one type"
    if let d = all.defaults.find(d -> d.shows == v.shows and d.device == device) {
        d.chosen = v
    } else {
        all.defaults.insert(Default(shows: v.shows, device: device, chosen: v))
    }
}

/// Adds the view a message sent to the person's own. Its author is who sent the message, as
/// the log recorded them, never who adds it: what it does asks first unless the person trusts
/// that sender. It is a copy: a later change the sender makes to theirs does not reach it.
action add(all: Views, m: chat.Message) -> WrittenView (icon: .plus, label: "Add to my views") {
    require m.sent_view != "" "The message sends no view"
    require created(m)?.who != .none "Who sent the message is not known"
    let from = created(m)?.who ?? invoker
    all.items.insert(WrittenView(title: m.body.excerpt, definition: m.sent_view, author: from, kept: true))
}

/// Sends a view to a channel: anyone there can add it to their own views.
flow send(v: WrittenView, to: chat.TextChannel) {
    chat.send_view(to, v.title, v.definition)
}

/// Every view, the kept ones a place of their own, each opening in a window of its own.
view views_list(all: Views) {
    std.list(all, all.items, everything: "All views", empty: "No views yet")
}

/// One view in a list: its title, and what was asked.
view view_row(v: WrittenView) {
    row(v.title, secondary: v.question, icon: .eye)
}

/// A view's window: the view itself, live, under its title and its actions.
view view_page(v: WrittenView) {
    std.page(v, canvas: true) {
        embed(v.definition, author: v.author, invocation: .none)
    }
}

/// An agent's answer to "what's left for the release?", over the workspace's tasks.
scenario "a release answer" -> WrittenView {
    views.items.insert(WrittenView(title: "Left for the release", question: "What's left for the release?", definition: "view left_for_the_release() \{\n    card(tasks.tasks, count: tasks.tasks.items.count(t -> not t.done)) \{\n        list(tasks.tasks.items.filter(t -> not t.done)) \{ t ->\n            row(t)\n        \}\n    \}\n\}", author: invoker))
}
