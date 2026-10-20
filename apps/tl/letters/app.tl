app so.tilo.letters "Letters"

type Draft {
    to: Text
    subject: Text
    body: Text
    sent: Bool = false
}

type Desk {
    drafts: Collection<Draft>
    open: Option<Draft>
}

/// Starts a draft and opens it.
action new_draft(desk: Desk) -> Draft {
    let d = desk.drafts.insert(Draft(subject: "Untitled"))
    desk.open = d
    return d
}

/// Opens a draft in the editor.
action open_draft(desk: Desk, d: Draft) {
    desk.open = d
}

/// Throws a draft away; the editor closes when it was the open one.
action discard(desk: Desk, d: Draft) {
    desk.drafts.remove(d)
    if desk.open.is_some() {
        if desk.open.unwrap() == d {
            desk.open = none
        }
    }
}

/// Posts the letter. This leaves the system, so it always asks first, showing the letter.
action send(d: Draft) proposal compose(d) {
    if not d.sent {
        outside("post", (d.to, d.subject, d.body))
        d.sent = true
    }
}

view compose(d: Draft) {
    stack(gap: related) {
        heading("Send to {d.to}")
        text(d.subject, style: "caption")
        text(d.body)
    }
}

view main(desk: Desk) {
    page(full) {
        split(side: leading, collapse: navigate) {
            primary(min: 14rem) {
                draft_list(desk)
            }
            secondary(min: 24rem) {
                if desk.open.is_some() {
                    editor(desk, desk.open.unwrap())
                }
            }
        }
    }
}

view draft_list(desk: Desk) {
    stack {
        row(distribute: between) {
            heading("Letters")
            button("New letter", on: new_draft(desk)) #new
        }
        scroll {
            for d in desk.drafts {
                draft_row(desk, d)
            }
        }
    }
}

view draft_row(desk: Desk, d: Draft) {
    button(d.subject, on: open_draft(desk, d))
    if not space.compact {
        text(d.to, style: "caption", truncate)
    }
}

view editor(desk: Desk, d: Draft) {
    stack(gap: section) {
        input(bind: d.to, placeholder: "To")
        input(bind: d.subject, placeholder: "Subject")
        input(bind: d.body, placeholder: "Write your letter", fill)
        row(distribute: end) {
            button("Discard", on: discard(desk, d)) #discard
            button("Send", on: send(d), enabled: not d.sent) #send
        }
    }
}
