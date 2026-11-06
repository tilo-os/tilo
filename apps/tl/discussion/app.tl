app so.tilo.lang.discussion "Discussion" (icon: .message_square, category: .office)

/// What someone said about one resource: a task, a note, a mail, a day. A comment is made from
/// the resource it is about (`created(c)?.from`), which puts it in that resource's thread; and
/// because it is made from it, it is a change to it for whoever made or read that resource, so
/// the row of the task it is about, and the place holding it, count a new comment the way they
/// count an edit.
///
/// `(attached)` says what the rest of a workspace should think of these records: they are about
/// what they were made from, never work made from it, so `linked` leaves them to the thread
/// that draws them.
type Comment (attached) {
    text: RichText (body)
}

/// Every comment of the workspace, in the order they were made.
type Comments = Collection<Comment>

root comments: Comments

default view recently for comments

row view remark for Comment

/// Says something about the resource the comment is made from: the thread under a task, a note
/// or a mail says it with `.made_from(r)`, so the comment records what it is about. An agent
/// says something the same way, its comment its own as any person's is.
action comment(all: Comments, text: RichText) -> Comment (icon: .message_square, label: "Comment") {
    require not text.is_empty() "Write a comment first"
    all.insert(Comment(text: text))
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
    all.remove(c)
}

/// What people said about `about`, oldest first — each with the face of who said it, when, what
/// they said, and that they changed it — and the field to say more. A comment someone else made
/// since the person last read `about` stands under the line saying so.
///
/// It takes the resource as the host's `Resource`, any resource of the workspace, so an app
/// that has something to discuss places it in one line: a task's page and a note's view
/// `use` this app and call `thread(t)`.
view thread(about: host.Resource) {
    lens draft: RichText = RichText("")
    lens editing: Option<Comment> = .none
    lens revised: RichText = RichText("")
    let all = comments
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
            comment(all, draft).made_from(about),
        )
    }
}

/// One comment of a thread: who said it and when over what they said, and what its author can do
/// to it while they are over it.
view comment_row(c: Comment, editing: State<Option<Comment>>, revised: State<RichText>) {
    let made = created(c)
    stack(gap: .tight) {
        row(gap: .tight) {
            avatar(made?.who)
            if let who = made?.who {
                text(who.name(), role: .label)
            }
            if let at = made?.at {
                ui.Time(at, role: .caption, secondary: true)
            }
        }
        if let e = editing {
            if e == c {
                revision(c, editing, revised)
            } else {
                written(c)
            }
        } else {
            written(c)
        }
        if created(c)?.who == .some(workspace.me) {
            ui.HoverToolbar {
                ui.Button("Edit", icon: .pencil, icon_only: true) {
                    on press: set(revised, c.text).then(set(editing, c))
                }
                button(delete(comments, c))
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

/// A comment being edited in place: what it says in an editor, saved or left as it was.
view revision(c: Comment, editing: State<Option<Comment>>, revised: State<RichText>) {
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
}

/// Everything said lately, each remark under the resource it is about: the window of the app
/// itself, where a thread is found by what it is about and opens it.
view recently(all: Comments) {
    std.list(all, all, empty: "Nothing said yet")
}

/// One comment as a row: the title of what it is about, and what was said about it; pressing it
/// opens what it is about.
view remark(c: Comment) {
    ui.ListRow(
        if let about = created(c)?.from { meta(about).title } else { meta(c).title },
        secondary: meta(c).title,
        item: created(c)?.from,
    )
}
