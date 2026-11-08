app so.tilo.lang.discussion "Discussion" (icon: .message_square, category: .office)

/// What someone said about one resource: a task, a note, a mail, a day. A comment is made from
/// the resource it is about (`created(c)?.from`), which puts it in that resource's thread; and
/// because it is made from it, it is a change to it for whoever made or read that resource, so
/// the row of the task it is about, and the place holding it, count a new comment the way they
/// count an edit.
///
/// What the rest of a workspace makes of these records is the page's to say: a page that draws
/// the thread under a resource draws what is about it there, and leaves it out of `std.linked`
/// (`said_about`).
type Comment {
    text: RichText
}

/// Every comment of the workspace, in the order they were made.
type Comments {
    items: Collection<Comment>
}

root comments: Comments

default view recently for comments

default view comment_page for Comment

row view remark for Comment

/// Says something about the resource the comment is made from: the thread under a task, a note
/// or a mail says it with `.made_from(r)`, so the comment records what it is about. An agent
/// says something the same way, its comment its own as any person's is.
action comment(all: Comments, text: RichText) -> Comment (
    icon: .message_square,
    label: "Comment",
    did: "commented",
) {
    require not text.is_empty() "Write a comment first"
    all.items.insert(Comment(text: text))
}

/// Changes what a comment says. Only its author may.
action edit(c: Comment, text: RichText = c.text) (icon: .pencil, label: "Edit") {
    require created(c)?.who == .some(invoker) "Only its author can edit a comment"
    require not text.is_empty() "Write a comment first"
    c.text = text
}

/// Deletes a comment. Only its author may. The platform takes it back rather than asking first,
/// and the removal is a version of the comment, so who removed it stays answerable.
destructive action delete(all: Comments, c: Comment) (icon: .trash, label: "Delete") {
    require created(c)?.who == .some(invoker) "Only its author can delete a comment"
    all.items.remove(c)
}

/// What the thread under `about` draws: the comments made from it, which a page that draws the
/// thread leaves out of `std.linked` (`without`), so what is said about a resource is drawn
/// once. The query is `linked`'s, as `thread`'s is: what the index says links to `about` and is
/// one of these comments.
fn said_about(about: host.Resource) -> List<host.Resource> {
    let said = comments.items.filter(c -> created(c)?.from == .some(about))
    linked(about).filter(l -> said.any(c -> l.of == c)).map(l -> l.of)
}

/// What people said about `about`, oldest first — each with the face of who said it, when, what
/// they said, and that they changed it — and the field to say more. A comment someone else made
/// since the person last read `about` stands under the line saying so.
///
/// It takes the resource as the host's `Resource`, any resource of the workspace, so an app
/// that has something to discuss places it in one line: a task's page and a note's view
/// `use` this app and call `thread(t)`.
view thread(about: host.Resource) {
    state draft: RichText = RichText("")
    state editing: Option<Comment> = .none
    state revised: RichText = RichText("")
    let all = comments.items
    let said = all.filter(c -> created(c)?.from == .some(about))
    let unread = said.find(c -> not unseen(c).is_empty())
    section("Discussion") {
        for c in said {
            if let u = unread {
                if u == c {
                    ui.UnreadDivider()
                }
            }
            comment_row(c, editing, revised)
        }
        std.composer(
            draft,
            "Comment on this {meta(about).noun}",
            comment(comments, draft).made_from(about),
        )
    }
}

/// One comment of a thread: who said it and when over what they said — in an editor while its
/// author changes it — and what they can do to it while they are over it. What it says stands
/// at the name's left edge, not the face's, as a conversation's lines and a history's steps do.
view comment_row(c: Comment, editing: State<Option<Comment>>, revised: State<RichText>) {
    let made = created(c)
    row(gap: .tight, align: .start) {
        avatar(made?.who)
        // What the comment holds, under the line naming who said it: the face stands beside the
        // whole of it rather than over it.
        stack(gap: .tight, fill: 1.0) {
            row(gap: .tight) {
                if let who = made?.who {
                    text(who.name(), role: .label)
                }
                if let at = made?.at {
                    ui.Time(at, role: .caption, secondary: true)
                }
            }
            if editing == .some(c) {
                rich_input(bind: revised, placeholder: "Edit comment") {
                    on submit: edit(c, revised).then(set(editing, .none))
                }
                row {
                    ui.Button("Cancel") {
                        on press: set(editing, .none)
                    }
                    ui.Button("Save", role: .primary) {
                        on press: edit(c, revised).then(set(editing, .none))
                    }
                }
            } else {
                written(c)
            }
            if made?.who == .some(workspace.me) {
                ui.HoverToolbar {
                    ui.Button("Edit", icon: .pencil, icon_only: true) {
                        on press: set(revised, c.text).then(set(editing, c))
                    }
                    button(delete(comments, c))
                }
            }
        }
    }
}

/// What a comment says, and that it was edited when its history holds an edit. References in it
/// draw as what they name.
view written(c: Comment) {
    rich_text(c.text)
    if history(c).any(v -> v.is(edit)) {
        text("edited", role: .caption, secondary: true)
    }
}

/// Everything said lately, each remark under the resource it is about, the one chosen opening
/// beside them as the thread it stands in: the window of the app itself, where a thread is found
/// by what it is about and opens it.
view recently(all: Comments) {
    // The comment this window's list shows opened where it stands.
    state opened: Option<Comment> = .none
    std.browse(all, all.items, opened, empty: "Nothing said yet", opens: false) { c ->
        remark(c)
    }
}

/// A comment's window: where it was said, which opens, and the thread it stands in — the same
/// thread that stands under what it is about. A comment made from nothing says what it says.
view comment_page(c: Comment) {
    let about = created(c)?.from
    std.document(c) {
        std.origin(c)
        if let at = about {
            thread(at)
        }
        if about == .none {
            written(c)
        }
    }
}

/// One comment as a row: the title of what it is about, and what was said about it.
view remark(c: Comment) {
    ui.ListRow(
        if let about = created(c)?.from { meta(about).title } else { meta(c).title },
        secondary: meta(c).title,
    )
}

/// Two days of a discussion: two people answering one another over what they are planning.
scenario "what people said lately" {
    let mia = person("Mia")
    let leo = person("Leo")
    let ask = invoke(
        comment(comments, RichText("Should the offsite be one day or two?")),
        by: mia,
        at: 2026-06-22T09:10Z,
    )
    invoke(
        comment(comments, RichText("Two: the travel eats a day on its own.")),
        by: leo,
        at: 2026-06-22T09:41Z,
        from: ask,
    )
    invoke(
        comment(comments, RichText("Agreed, I'll book the Thursday night.")),
        by: mia,
        at: 2026-06-23T08:05Z,
        from: ask,
    )
    let plan = invoke(
        comment(comments, RichText("Who books the room for the workshop?")),
        by: leo,
        at: 2026-06-24T16:20Z,
    )
    invoke(
        comment(comments, RichText("I can, once the dates are settled.")),
        by: mia,
        at: 2026-06-25T07:15Z,
        from: plan,
    )
}

/// A reply, with the thread it stands in under what it answers: what was asked, who answered and
/// when.
scenario "a reply to a comment" -> Comment {
    let mia = person("Mia")
    let leo = person("Leo")
    let ask = invoke(
        comment(comments, RichText("Do we renew the studio lease this year?")),
        by: mia,
        at: 2026-06-24T10:02Z,
    )
    invoke(
        comment(comments, RichText("Ask the neighbours what they pay first.")),
        by: leo,
        at: 2026-06-24T11:45Z,
        from: ask,
    )
}
