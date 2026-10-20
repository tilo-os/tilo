app so.tilo.letters "Letters"

type Draft {
    to: Text
    subject: Text
    body: Text
    sent: Bool = false
}

/// The workspace's letters.
type Desk {
    drafts: Collection<Draft>
}

/// Which letters the desk lists.
enum Shelf {
    /// Letters not sent yet.
    drafts
    /// Letters that went out.
    sent
}

root Desk

default view desk for Desk

default view editor for Draft

/// Starts a letter.
action new_draft(desk: Desk) -> Draft {
    desk.drafts.insert(Draft(subject: "Untitled"))
}

/// Throws a letter away.
action discard(desk: Desk, d: Draft) {
    desk.drafts.remove(d)
}

/// Posts the letter. This leaves the system, so it always asks first, showing the letter.
action send(d: Draft) proposal compose(d) {
    if not d.sent {
        outside("post", (d.to, d.subject, d.body))
        d.sent = true
    }
}

view compose(d: Draft) {
    stack(gap: .related) {
        heading("Send to {d.to}")
        text(d.subject, style: .caption)
        text(d.body)
    }
}

/// The desk's window: its letters, each opening in a window of its own. Which shelf it shows
/// is the window's.
view desk(desk: Desk) {
    window shelf: Shelf = .drafts
    page {
        stack(gap: .section) {
            row(distribute: .between) {
                heading("Letters")
                button("New letter", on: new_draft(desk), style: .primary) as new
            }
            row(gap: .related) {
                button("Drafts", on: set(shelf, .drafts), style: if shelf == .drafts { .primary } else { .plain }) as drafts
                button("Sent", on: set(shelf, .sent), style: if shelf == .sent { .primary } else { .plain }) as sent
            }
            scroll {
                stack(gap: .related) {
                    for d in desk.drafts.filter(d -> d.sent == (shelf == .sent)) {
                        draft_row(desk, d)
                    }
                }
            }
        }
    }
}

view draft_row(desk: Desk, d: Draft) {
    row(distribute: .between) {
        button(d.subject, on: open(d), style: .plain)
        if not space.compact {
            text(d.to, style: .caption, truncate)
        }
        button("Discard", on: discard(desk, d), style: .destructive)
    }
}

/// A letter's window.
view editor(d: Draft) {
    page {
        stack(gap: .section) {
            input(bind: d.to, placeholder: "To")
            input(bind: d.subject, placeholder: "Subject")
            input(bind: d.body, placeholder: "Write your letter", fill)
            row(distribute: .end) {
                button("Send", on: send(d), enabled: not d.sent, style: .primary) as send
            }
        }
    }
}
