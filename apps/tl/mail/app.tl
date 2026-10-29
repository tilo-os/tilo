app so.tilo.lang.mail "Mail" (icon: .mail, category: .office, records: .own)

/// What the server of one of the workspace's mail accounts holds. The account itself, its
/// address, servers and signature, is the workspace settings', and its credentials stay in
/// custody; the system keeps this in step with the server.
type Account (origin: imap.account) {
    key: Text merge register
    address: Text (title)
    name: Text
    trouble: Text
    online: Bool
    mailboxes: Collection<Mailbox>
    threads: Collection<Thread>
}

/// A mailbox of an account.
type Mailbox (origin: imap.mailbox) {
    key: Text merge register
    name: Text (title)
    role: Text merge register
    unread: Int
    total: Int
}

/// A conversation: messages that answer each other, newest last.
type Thread (origin: imap.thread) {
    key: Text merge register
    account: Text merge register
    subject: Text (title)
    latest: Time
    snippet: Text (body)
    senders: Text
    count: Int
    unread: Bool
    flagged: Bool
    boxes: Set<Text>
    messages: Collection<Message>

    search {
        filter "Inbox" = boxes.contains("inbox") (icon: .inbox)
        filter flagged "Flagged" (icon: .flag)
        filter "Sent" = boxes.contains("sent") (icon: .send)
        filter "Archive" = boxes.contains("archive") (icon: .archive)
        filter "Junk" = boxes.contains("junk") (icon: .shield)
        filter "Trash" = boxes.contains("trash") (icon: .trash)
        filter unread "Unread" (icon: .mail)
        sort latest newest
        hide key
        hide account
        hide boxes
    }
}

/// A message.
type Message (origin: imap.message) {
    key: Text merge register
    account: Text merge register
    subject: Text (title)
    from: Text
    from_name: Text
    to: Text
    cc: Text
    received: Time
    snippet: Text
    body: Text (body)
    message_id: Text merge register
    references: Text merge register
    attachments: Text
    unread: Bool
    flagged: Bool
    mailbox: Text
}

/// A message being written: who it goes to, what it says, what it answers, and whether it
/// went. The account it goes from is named by its key.
type Draft {
    from: Text
    to: Text
    cc: Text
    subject: Text (title)
    body: Text (body)
    in_reply_to: Text
    references: Text
    sent: Bool = false

    search {
        filter "Drafts" = not sent (icon: .pencil)
        filter sent (icon: .send)
        hide in_reply_to
        hide references
    }
}

/// The person's mail: the accounts the workspace's settings hold, and what they are writing.
type Mail {
    accounts: Collection<Account>
    drafts: Collection<Draft>
}

root Mail

default view mail for Mail

default view conversation for Thread

default view editor for Draft

row view thread_row for Thread

row view draft_row for Draft

/// Starts a message from the account.
primary action new_message(a: Account) -> Draft (icon: .pencil, label: "New message") {
    let m = root Mail
    m.drafts.insert(Draft(from: a.key))
}

/// Starts a message from the first account.
primary action new_draft(m: Mail) -> Draft (icon: .pencil, label: "New message") {
    require not m.accounts.is_empty() "Add a mail account in Settings first"
    let from = m.accounts.first()?.key ?? ""
    m.drafts.insert(Draft(from: from))
}

/// The subject an answer to a conversation has.
fn re(subject: Text) -> Text {
    "Re: {subject}"
}

/// Answers the conversation's newest message, to whoever wrote it.
primary action reply(t: Thread) -> Draft (icon: .reply) {
    let m = root Mail
    let last = t.messages.last()
    m.drafts.insert(Draft(from: t.account, to: last?.from ?? "", subject: re(t.subject), in_reply_to: last?.message_id ?? "", references: "{last?.references ?? ""} {last?.message_id ?? ""}"))
}

/// Answers the conversation's newest message, to everyone it went to.
action reply_all(t: Thread) -> Draft (icon: .reply_all, label: "Reply all") {
    let m = root Mail
    let last = t.messages.last()
    m.drafts.insert(Draft(from: t.account, to: last?.from ?? "", cc: if let l = last {
        if l.cc == "" { l.to } else { "{l.to}, {l.cc}" }
    } else { "" }, subject: re(t.subject), in_reply_to: last?.message_id ?? "", references: "{last?.references ?? ""} {last?.message_id ?? ""}"))
}

/// Sends the conversation's newest message on, quoted.
action forward(t: Thread) -> Draft (icon: .forward) {
    let m = root Mail
    let last = t.messages.last()
    let said = if let l = last {
        "\n\nFrom {l.from_name} <{l.from}>:\n\n{l.body}"
    } else { "" }
    m.drafts.insert(Draft(from: t.account, subject: "Fwd: {t.subject}", body: said))
}

/// Archives the conversation: its messages leave the inbox.
action archive(t: Thread) (icon: .archive) {
    require t.boxes.any(b -> b == "inbox") "It is not in the inbox"
    for m in t.messages {
        if m.mailbox == "inbox" {
            m.mailbox = "archive"
        }
    }
    t.boxes.remove("inbox")
    t.boxes.add("archive")
}

/// Moves the conversation back to the inbox.
action to_inbox(t: Thread) (icon: .inbox, label: "Move to inbox") {
    require not t.boxes.any(b -> b == "inbox") "It is in the inbox"
    for m in t.messages {
        if m.mailbox != "sent" {
            m.mailbox = "inbox"
        }
    }
    t.boxes.remove("archive")
    t.boxes.remove("trash")
    t.boxes.remove("junk")
    t.boxes.add("inbox")
}

/// Moves the conversation to another mailbox of its account.
action move_to(t: Thread, to: Mailbox) (label: "Move to…", icon: .folder) {
    require not t.boxes.any(b -> b == to.key) "It is in {to.name} already"
    for m in t.messages {
        if m.mailbox != "sent" {
            m.mailbox = to.key
        }
    }
    t.boxes.remove("inbox")
    t.boxes.remove("archive")
    t.boxes.remove("junk")
    t.boxes.remove("trash")
    t.boxes.add(to.key)
}

/// Deletes the conversation: its messages go to the trash.
destructive action delete(t: Thread) (icon: .trash) {
    require not t.boxes.any(b -> b == "trash") "It is in the trash"
    for m in t.messages {
        m.mailbox = "trash"
    }
    t.boxes.remove("inbox")
    t.boxes.remove("archive")
    t.boxes.remove("sent")
    t.boxes.remove("junk")
    t.boxes.add("trash")
}

/// Flags or unflags the conversation.
action flag(t: Thread, flagged: Bool = not t.flagged) (label: if t.flagged { "Unflag" } else { "Flag" }, icon: .flag, on: t.flagged) {
    if let last = t.messages.last() {
        last.flagged = flagged
    }
    if not flagged {
        for m in t.messages {
            m.flagged = false
        }
    }
    t.flagged = flagged
}

/// Marks the conversation read, or unread again.
action mark_read(t: Thread, unread: Bool = not t.unread) (label: if t.unread { "Mark as read" } else { "Mark as unread" }, icon: .mail_open, on: not t.unread) {
    if t.unread != unread {
        if unread {
            if let last = t.messages.last() {
                last.unread = true
            }
        } else {
            for m in t.messages {
                if m.unread {
                    m.unread = false
                }
            }
        }
        t.unread = unread
    }
}

/// Throws a message being written away.
destructive action discard(m: Mail, d: Draft) (icon: .trash) {
    m.drafts.remove(d)
}

/// Sends the message from its account. It leaves the system, so it goes once, and waits in
/// the outbox until the server took it.
primary action send(d: Draft) proposal compose(d) (icon: .send) {
    require not d.sent "This message went already"
    require d.to != "" "It goes to nobody yet"
    outside("smtp.send", (d.from, d.to, d.cc, d.subject, d.body, d.in_reply_to, d.references))
    d.sent = true
}

/// The message as it will leave, shown when someone is asked to send it.
view compose(d: Draft) {
    stack(gap: .related) {
        text("To {d.to}", role: .title)
        text(d.subject, role: .caption)
        text(d.body)
    }
}

/// The mail window: the first account's conversations, the inbox first, each opening beside
/// the list. The mailboxes are the conversations' places.
view mail(m: Mail) {
    if let a = m.accounts.first() {
        mailboxes(a)
    } else {
        std.page(m) {
            ui.EmptyState("Add a mail account in Settings", icon: .mail)
        }
    }
}

/// An account's window: its mailboxes in the sidebar with how many unread each holds, the
/// conversations of the one chosen, newest first, and the one open beside them. Opening a
/// conversation reads it. The mailbox is the window's, as people type it in search
/// (`is:inbox`), so everyone looking sees the same list; it starts on the inbox.
view mailboxes(a: Account) {
    window query: Text = "is:inbox"
    let places = filters(Thread).filter(f -> f.toggle)
    let shown = a.threads.matching(query)
    let title = places.find(p -> p.query == query)?.label ?? "All mail"
    ui.NavShell(content_kind: .document, sidebar_title: .some(a.address), overview_title: .some(title), overview_count: .some(shown.len())) {
        sidebar: {
            ui.NavList(selected: if query == "" { "all" } else { query }) {
                for p in places {
                    let unread = a.threads.matching("{p.query} is:unread").len()
                    ui.NavItem(p.query, p.label, icon: p.icon, count: if unread > 0 { .some(unread) } else { .none }) {
                        on select: set(query, p.query)
                    }
                }
                ui.NavItem("all", "All mail", icon: .mail) {
                    on select: set(query, "")
                }
            }
        }
        overview_actions: {
            std.header(actions(a))
        }
        overview: {
            if shown.is_empty() {
                ui.EmptyState("Nothing here", icon: .inbox)
            } else {
                ui.VirtualList(selection_follows_focus: true, in_column: true) {
                    on select(x: Thread): open(x, front: false).then(mark_read(x, false))
                    on activate(x: Thread): open(x).then(mark_read(x, false))
                    for x in shown {
                        let offered = actions(x)
                        ui.ListItem(item: x, presence: lenses(x).find(l -> not l.mine)?.who) {
                            menu: {
                                for o in offered {
                                    ui.MenuItem(o.label, icon: o.icon, destructive: o.destructive) {
                                        on choose: o.run
                                    }
                                }
                            }
                            swipe: {
                                if let o = offered.find(.destructive) {
                                    ui.SwipeAction(o.label, icon: o.icon, destructive: true) {
                                        on swipe: o.run
                                    }
                                }
                            }
                            thread_row(x)
                        }
                    }
                }
            }
        }
        content: {
            ui.EmptyState("No conversation selected", icon: .mail)
        }
    }
}

/// One conversation in a list: who wrote, the subject, the start of the newest message and
/// when it came.
view thread_row(t: Thread) {
    row(t.senders, secondary: t.subject, meta: t.latest, mark: if t.flagged { .flag } else if t.unread { .mail })
}

/// One message being written in a list: its subject and who it goes to.
view draft_row(d: Draft) {
    row(d.subject, secondary: if let s = delivery(d) { delivery_says(s.state, s.reason) } else { d.to }, mark: if d.sent { .send })
}

/// How a message on its way out stands, in a sentence: waiting to go online, sending, sent,
/// not sent and why, or not known.
fn delivery_says(state: Text, reason: Text) -> Text {
    if state == "sent" { "Sent" } else if state == "sending" { "Sending" } else if state == "rejected" { "Not sent: {reason}" } else if state == "unknown" { "Not known whether it was sent" } else if reason == "" { "Waiting to send" } else { "Waiting to go online: {reason}" }
}

/// A conversation's window: its messages in order, each with who wrote it, to whom and when.
view conversation(t: Thread) {
    std.page(t) {
        for m in t.messages {
            stack(gap: .related) {
                ui.ListRow(title: m.from_name, secondary: m.to, meta: m.received)
                text(m.body)
            }
        }
    }
}

/// A message being written: its window.
view editor(d: Draft) {
    std.document(d) {
        if let s = delivery(d) {
            field("Status") {
                text(delivery_says(s.state, s.reason))
            }
        }
        field("To") {
            input(bind: d.to, placeholder: "name@example.org")
        }
        field("Cc") {
            input(bind: d.cc)
        }
        input(bind: d.subject, placeholder: "Subject")
        input(bind: d.body, placeholder: "Write your message", lines: 12)
    }
}

/// An inbox a working week in: conversations to read, some answered, one flagged, a few
/// archived, sent and junk.
scenario "a busy inbox" -> Thread {
    let m = root Mail
    let a = m.accounts.insert(Account(key: "sam@rivera.example", address: "sam@rivera.example", name: "Sam Rivera", online: true))
    a.mailboxes.insert(Mailbox(key: "inbox", name: "Inbox", role: "inbox", unread: 3, total: 7))
    a.mailboxes.insert(Mailbox(key: "archive", name: "Archive", role: "archive", total: 2))
    a.mailboxes.insert(Mailbox(key: "sent", name: "Sent", role: "sent", total: 1))
    a.mailboxes.insert(Mailbox(key: "drafts", name: "Drafts", role: "drafts"))
    a.mailboxes.insert(Mailbox(key: "junk", name: "Junk", role: "junk", total: 1))
    a.mailboxes.insert(Mailbox(key: "trash", name: "Trash", role: "trash"))
    let launch = a.threads.insert(Thread(key: "launch@rivera.example", account: a.key, subject: "Launch plan for the autumn release", latest: 2026-06-30T13:40Z, snippet: "Agreed on the twelfth. I will tell support today so they can prepare the help pages.", senders: "Ada Lovelace, Grace Hopper", count: 3, unread: true))
    launch.boxes.add("inbox")
    launch.messages.insert(Message(key: "launch@rivera.example", account: a.key, subject: "Launch plan for the autumn release", from: "grace@hopper.example", from_name: "Grace Hopper", to: "sam@rivera.example, ada@analytical.example", received: 2026-06-30T09:12Z, snippet: "Here is the draft plan.", body: "Hi both,\n\nHere is the draft plan for the autumn release. The beta goes out on the fifth, the release on the twelfth if nothing turns up.\n\nCould you each look at your part by Wednesday?\n\nGrace", message_id: "launch@rivera.example", mailbox: "inbox"))
    launch.messages.insert(Message(key: "launch-2@rivera.example", account: a.key, subject: "Re: Launch plan for the autumn release", from: "sam@rivera.example", from_name: "Sam Rivera", to: "grace@hopper.example, ada@analytical.example", received: 2026-06-30T11:02Z, snippet: "The fifth works for the beta.", body: "The fifth works for the beta. I would move the release to the fourteenth so the docs can catch up.\n\nSam", message_id: "launch-2@rivera.example", references: "launch@rivera.example", mailbox: "sent"))
    launch.messages.insert(Message(key: "launch-3@rivera.example", account: a.key, subject: "Re: Launch plan for the autumn release", from: "ada@analytical.example", from_name: "Ada Lovelace", to: "sam@rivera.example, grace@hopper.example", received: 2026-06-30T13:40Z, snippet: "Agreed on the twelfth.", body: "Agreed on the twelfth. I will tell support today so they can prepare the help pages.\n\nAda", message_id: "launch-3@rivera.example", references: "launch@rivera.example launch-2@rivera.example", unread: true, mailbox: "inbox"))
    launch.boxes.add("sent")
    let lease = a.threads.insert(Thread(key: "lease@lettings.example", account: a.key, subject: "Your lease renewal", latest: 2026-06-30T10:25Z, snippet: "The new lease is attached. Please sign by Friday so we can register it before the month ends.", senders: "Mill Lane Lettings", count: 1, unread: true, flagged: true))
    lease.boxes.add("inbox")
    lease.messages.insert(Message(key: "lease@lettings.example", account: a.key, subject: "Your lease renewal", from: "office@lettings.example", from_name: "Mill Lane Lettings", to: "sam@rivera.example", received: 2026-06-30T10:25Z, snippet: "The new lease is attached.", body: "Dear Sam,\n\nThe new lease is attached. Please sign by Friday so we can register it before the month ends.\n\nKind regards,\nMill Lane Lettings", message_id: "lease@lettings.example", attachments: "Lease 2026.pdf", unread: true, flagged: true, mailbox: "inbox"))
    let dinner = a.threads.insert(Thread(key: "dinner@bo.example", account: a.key, subject: "Dinner on Friday?", latest: 2026-06-30T08:51Z, snippet: "Shall we try the new place on Mill Lane? I can book for eight.", senders: "Bo Chen", count: 1, unread: true))
    dinner.boxes.add("inbox")
    dinner.messages.insert(Message(key: "dinner@bo.example", account: a.key, subject: "Dinner on Friday?", from: "bo@chen.example", from_name: "Bo Chen", to: "sam@rivera.example", received: 2026-06-30T08:51Z, snippet: "Shall we try the new place?", body: "Shall we try the new place on Mill Lane? I can book for eight.\n\nBo", message_id: "dinner@bo.example", unread: true, mailbox: "inbox"))
    let review = a.threads.insert(Thread(key: "review@code.example", account: a.key, subject: "Review requested: importer handles merged cells", latest: 2026-06-29T17:05Z, snippet: "Chen asked you to review the change to the table importer.", senders: "Forge", count: 1))
    review.boxes.add("inbox")
    review.messages.insert(Message(key: "review@code.example", account: a.key, subject: "Review requested: importer handles merged cells", from: "notifications@code.example", from_name: "Forge", to: "sam@rivera.example", received: 2026-06-29T17:05Z, snippet: "Chen asked you to review.", body: "Chen asked you to review the change to the table importer: merged cells are kept, and the column widths follow the source.", message_id: "review@code.example", mailbox: "inbox"))
    let library = a.threads.insert(Thread(key: "library@city.example", account: a.key, subject: "Two books due next week", latest: 2026-06-29T09:30Z, snippet: "Piranesi and The Dispossessed are due on the seventh. You can renew them twice more.", senders: "City Library", count: 1))
    library.boxes.add("inbox")
    library.messages.insert(Message(key: "library@city.example", account: a.key, subject: "Two books due next week", from: "loans@city.example", from_name: "City Library", to: "sam@rivera.example", received: 2026-06-29T09:30Z, snippet: "Two books are due.", body: "Piranesi and The Dispossessed are due on the seventh. You can renew them twice more.", message_id: "library@city.example", mailbox: "inbox"))
    let photos = a.threads.insert(Thread(key: "photos@mia.example", account: a.key, subject: "Photos from the lake", latest: 2026-06-28T19:44Z, snippet: "The sunset ones came out best. I put the rest in the shared album.", senders: "Mia Novak", count: 1))
    photos.boxes.add("inbox")
    photos.messages.insert(Message(key: "photos@mia.example", account: a.key, subject: "Photos from the lake", from: "mia@novak.example", from_name: "Mia Novak", to: "sam@rivera.example", received: 2026-06-28T19:44Z, snippet: "The sunset ones came out best.", body: "The sunset ones came out best. I put the rest in the shared album.\n\nMia", message_id: "photos@mia.example", attachments: "lake-1.jpg\nlake-2.jpg", mailbox: "inbox"))
    let invoice = a.threads.insert(Thread(key: "invoice@studio.example", account: a.key, subject: "Invoice for June", latest: 2026-06-27T08:00Z, snippet: "Thanks for the work this month. The invoice is attached, payable within thirty days.", senders: "North Studio", count: 1))
    invoice.boxes.add("archive")
    invoice.messages.insert(Message(key: "invoice@studio.example", account: a.key, subject: "Invoice for June", from: "accounts@studio.example", from_name: "North Studio", to: "sam@rivera.example", received: 2026-06-27T08:00Z, snippet: "The invoice is attached.", body: "Thanks for the work this month. The invoice is attached, payable within thirty days.", message_id: "invoice@studio.example", attachments: "Invoice 0612.pdf", mailbox: "archive"))
    let offer = a.threads.insert(Thread(key: "offer@deals.example", account: a.key, subject: "You have won a cruise", latest: 2026-06-26T03:12Z, snippet: "Claim your prize within 24 hours by confirming your card details.", senders: "Prize Desk", count: 1))
    offer.boxes.add("junk")
    offer.messages.insert(Message(key: "offer@deals.example", account: a.key, subject: "You have won a cruise", from: "win@deals.example", from_name: "Prize Desk", to: "sam@rivera.example", received: 2026-06-26T03:12Z, snippet: "Claim your prize.", body: "Claim your prize within 24 hours by confirming your card details.", message_id: "offer@deals.example", mailbox: "junk"))
    launch
}

/// No account yet: what a first start shows.
scenario "no account yet" {
    root Mail
}
