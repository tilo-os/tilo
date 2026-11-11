app so.tilo.lang.mail "Mail" (icon: .mail, category: .office, records: .own)

/// What the server of one of the workspace's mail accounts holds. The account itself, its
/// address, servers and signature, is the workspace settings', and its credentials stay in
/// custody; the system keeps this in step with the server.
type Account (origin: imap.account) {
    key: Text merge register
    address: Text
    name: Text
    trouble: Text
    online: Bool
    /// When the account's mail was last taken in from its server, as the clock the person read
    /// it at: 1970 when it never was.
    fetched: Time = Time("1970-01-01T00:00:00Z")
    /// What the account's messages end with: the person's own, per account, as every mail
    /// client signs a message. Empty for none.
    signature: Text
    mailboxes: Collection<Mailbox>
    threads: Collection<Thread>
}

/// A mailbox of an account.
type Mailbox (origin: imap.mailbox) {
    key: Text merge register
    name: Text
    role: Text merge register
    unread: Int
    total: Int
}

/// A conversation: messages that answer each other, newest last.
type Thread (origin: imap.thread) {
    key: Text merge register
    account: Text merge register
    subject: Text
    latest: Time
    snippet: Text
    senders: Text
    count: Int
    unread: Bool
    flagged: Bool
    boxes: Set<Text>
    messages: Collection<Message>
    /// The labels people gave it (Gmail's and Fastmail's): the person's own, never the server's.
    labels: Set<Text>
    /// Snoozed until this moment: out of the inbox until then, and back once it has come, with
    /// no window open. The epoch when it is not snoozed.
    snoozed_until: Time = Time("1970-01-01T00:00:00Z")
    /// Muted: the conversation stays out of the inbox while new messages keep coming, as a
    /// busy conversation is silenced, and it is still found under Muted and All mail.
    muted: Bool = false

    search {
        filter "Inbox" = boxes.contains("inbox")
            and snoozed_until <= now
            and not muted (icon: .inbox)
        filter "Snoozed" = snoozed_until > now (icon: .clock)
        filter labels (icon: .tag)
        filter flagged "Flagged" (icon: .flag)
        filter muted "Muted" (icon: .bell_off)
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

/// A label the person gave their conversations: their own, never the server's, as Gmail's and
/// Fastmail's are. A conversation holds its labels by name, so giving one twice makes it once.
type Label {
    name: Text

    search {
        sort name a_z
    }
}

/// A message.
type Message (origin: imap.message) {
    key: Text merge register
    account: Text merge register
    subject: Text
    from: Text
    from_name: Text
    to: Text
    cc: Text
    received: Time
    snippet: Text
    body: Text
    message_id: Text merge register
    references: Text merge register
    attachments: Text
    unread: Bool
    flagged: Bool
    mailbox: Text
}

/// A message being written: who it goes to, what it says, what it answers, and how it stands on
/// its way out. The account it goes from is named by its key.
type Draft (title: subject, untitled: "New message") {
    from: Text
    to: Text
    cc: Text
    subject: Text
    body: Text
    in_reply_to: Text
    references: Text
    sent: Bool = false
    /// When it goes: 1970 for a draft not sent yet.
    send_at: Time = Time("1970-01-01T00:00:00Z")
    /// It was taken back before it went: it never leaves, and it is a draft again.
    recalled: Bool = false

    search {
        filter "Drafts" = not sent and not recalled (icon: .pencil)
        filter sent (icon: .send)
        hide in_reply_to
        hide references
        hide send_at
    }
}

/// The person's mail: the accounts the workspace's settings hold, what they are writing, and
/// the labels they gave their conversations.
type Mail {
    accounts: Collection<Account>
    drafts: Collection<Draft>
    labels: Collection<Label>
    /// How many seconds a sent message waits before it goes, so it can be taken back: the
    /// person's setting, ten by default, and no wait at all when it is zero.
    hold: Int = 10
}

root Mail

default view mail for Mail

default view conversation for Thread

default view mailboxes for Account

default view inbox for Mailbox

default view editor for Draft

row view thread_row for Thread

row view draft_row for Draft

/// The signature the account `key` signs its messages with: its own, and empty when it has
/// none or the account is not one of the person's.
fn signature_of(m: Mail, key: Text) -> Text {
    m.accounts.find(a -> a.key == key)?.signature ?? ""
}

/// `body` with the account's signature over it, as every mail client signs a message: the
/// signature, a blank line, then what follows it (a quotation). `body` alone when the account
/// has no signature.
fn signed(m: Mail, key: Text, body: Text) -> Text {
    let signature = signature_of(m, key)
    if signature == "" { body } else { "{signature}{body}" }
}

/// Sets what the account signs its messages with, or clears it: the person's own, per account.
action set_signature(a: Account, signature: Text) (icon: .pen_tool, label: "Signature") {
    a.signature = signature
}

/// Asks the account's server for its mail now: what a person presses when the window says the
/// server could not be reached, or that what it fetched could not be saved. The account says
/// how it stands once it answers.
action check_now(a: Account) (icon: .refresh, label: "Check now") {
    outside("imap.check", a.key)
}

/// Starts a message from the account.
primary action new_message(a: Account) -> Draft (icon: .pencil, label: "New message") {
    let m = root Mail
    m.drafts.insert(Draft(from: a.key, body: signature_of(m, a.key)))
}

/// Starts a message from the first account.
primary action new_draft(m: Mail) -> Draft (icon: .pencil, label: "New message") {
    require not m.accounts.is_empty() "Add a mail account in Settings first"
    applies not m.accounts.is_empty() "Add a mail account in Settings first"
    let from = m.accounts.first()?.key ?? ""
    m.drafts.insert(Draft(from: from, body: signature_of(m, from)))
}

/// The subject an answer to a conversation has.
fn re(subject: Text) -> Text {
    "Re: {subject}"
}

/// What a message says, its lines ended one way: a server's mail ends them either way, and
/// what a reader sees should not depend on which.
fn as_lines(body: Text) -> Text {
    body.replace("\r\n", "\n")
}

/// The clock time a moment falls at, as people read one: `10:41` or `10:41 AM`, in the clock the
/// person chose in Settings, as every other time the window shows reads. What the window says
/// when an account's mail was last read from its server; an account whose mail never came says
/// nothing, its moment standing at the epoch.
fn fetched_clock(a: Account) -> Text {
    let never = Time("1970-01-01T00:00:00Z")
    if a.fetched <= never {
        return ""
    }
    a.fetched.clock()
}

/// Where the quotation in what a message says begins: the blank line above the line naming who
/// wrote it and when ("On 30 Jun 2026, Ada wrote:"), as Gmail and Apple Mail read one. None when
/// it quotes nothing.
fn quote_begins(body: Text) -> Option<Int> {
    body.find("\n\nOn ")
}

/// `m` quoted as a reply quotes it: a blank line, the line naming who wrote it, then its lines
/// after `> `, as Gmail answers.
fn quoted(m: Message) -> Text {
    "\n\nOn {m.received}, {m.from_name} wrote:\n> {as_lines(m.body).replace("\n", "\n> ")}\n"
}

/// Answers the conversation's newest message, to whoever wrote it, quoting it under what is
/// written.
primary action reply(t: Thread) -> Draft (icon: .reply, key: "r") {
    let m = root Mail
    let last = t.messages.last()
    m.drafts.insert(
        Draft(
            from: t.account,
            to: last?.from ?? "",
            subject: re(t.subject),
            body: signed(m, t.account, if let l = last { quoted(l) } else { "" }),
            in_reply_to: last?.message_id ?? "",
            references: "{last?.references ?? ""} {last?.message_id ?? ""}",
        ),
    )
}

/// Answers the conversation's newest message, to everyone it went to, quoting it.
action reply_all(t: Thread) -> Draft (icon: .reply_all, label: "Reply all", key: "a") {
    let m = root Mail
    let last = t.messages.last()
    m.drafts.insert(
        Draft(
            from: t.account,
            to: last?.from ?? "",
            cc: if let l = last {
                if l.cc == "" { l.to } else { "{l.to}, {l.cc}" }
            } else { "" },
            subject: re(t.subject),
            body: signed(m, t.account, if let l = last { quoted(l) } else { "" }),
            in_reply_to: last?.message_id ?? "",
            references: "{last?.references ?? ""} {last?.message_id ?? ""}",
        ),
    )
}

/// Answers one message of the conversation, to whoever wrote it, quoting it.
action reply_to(m: Message) -> Draft (icon: .reply) {
    let mail = root Mail
    mail.drafts.insert(
        Draft(
            from: m.account,
            to: m.from,
            subject: re(m.subject),
            body: signed(mail, m.account, quoted(m)),
            in_reply_to: m.message_id,
            references: "{m.references} {m.message_id}",
        ),
    )
}

/// Answers one message of the conversation, to everyone it went to but the person answering.
action reply_all_to(m: Message) -> Draft (icon: .reply_all, label: "Reply all") {
    let mail = root Mail
    let theirs = if m.cc == "" { m.to } else { "{m.to}, {m.cc}" }
    mail.drafts.insert(
        Draft(
            from: m.account,
            to: m.from,
            cc: theirs,
            subject: re(m.subject),
            body: signed(mail, m.account, quoted(m)),
            in_reply_to: m.message_id,
            references: "{m.references} {m.message_id}",
        ),
    )
}

/// Sends one message of the conversation on, with its sender, when and what it says.
action forward_message(m: Message) -> Draft (icon: .forward) {
    let mail = root Mail
    mail.drafts.insert(
        Draft(
            from: m.account,
            subject: "Fwd: {m.subject}",
            body: signed(
                mail,
                m.account,
                "\n\nFrom {m.from_name} <{m.from}> ({m.received}):\n\n> {m.body.replace("\n", "\n> ")}\n",
            ),
        ),
    )
}

/// Sends the conversation's newest message on, with its sender and what it says.
action forward(t: Thread) -> Draft (icon: .forward, key: "f") {
    let m = root Mail
    let last = t.messages.last()
    let said = if let l = last {
        "\n\nFrom {l.from_name} <{l.from}> ({l.received}):\n\n> {l.body.replace("\n", "\n> ")}\n"
    } else { "" }
    m.drafts.insert(
        Draft(from: t.account, subject: "Fwd: {t.subject}", body: signed(m, t.account, said)),
    )
}

/// Archives the conversation: its messages leave the inbox.
action archive(t: Thread) (icon: .archive, key: "e") {
    require t.boxes.any(b -> b == "inbox") "It is not in the inbox"
    for m in t.messages {
        if m.mailbox == "inbox" {
            m.mailbox = "archive"
        }
    }
    t.boxes.remove("inbox")
    t.boxes.add("archive")
}

/// Moves the conversation back to the inbox, and unmutes it: what a person brings back is what
/// they want to see again.
action to_inbox(t: Thread) (icon: .inbox, label: "Move to inbox", key: "i") {
    require not t.boxes.any(b -> b == "inbox") or t.muted "It is in the inbox"
    for m in t.messages {
        if m.mailbox != "sent" {
            m.mailbox = "inbox"
        }
    }
    t.boxes.remove("archive")
    t.boxes.remove("trash")
    t.boxes.remove("junk")
    t.boxes.add("inbox")
    t.muted = false
}

/// Moves the conversation to another mailbox of its account.
action move_to(t: Thread, to: Mailbox) (label: "Move to", icon: .folder) {
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
destructive action delete(t: Thread) (icon: .trash, key: "#") {
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
action flag(t: Thread, flagged: Bool = not t.flagged) (
    label: if t.flagged { "Unflag" } else { "Flag" },
    icon: .flag,
    on: t.flagged,
    key: "s",
) {
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

/// Puts the conversation away until a moment: it leaves the inbox, and comes back once that
/// moment has come, with no window open. The moment is a field, so it survives closing and
/// reopening, and an agent snoozes the same way.
action snooze(t: Thread, until: Time) uses time (icon: .clock, label: "Snooze") {
    require until > now "Pick a moment still to come"
    t.snoozed_until = until
}

/// Puts the conversation away for an hour, the snooze people reach for most.
action snooze_an_hour(t: Thread) uses time (icon: .clock, label: "Snooze an hour") {
    snooze(t, now + 1h)
}

/// Puts the conversation away until tomorrow morning, at nine, in the person's own zone.
action snooze_until_tomorrow(t: Thread) uses time (icon: .clock, label: "Snooze until tomorrow") {
    snooze(t, Time.parse("tomorrow 9am", near: now) ?? now + 18h)
}

/// Brings a snoozed conversation back now, whatever its moment was: it is in the inbox again.
action unsnooze(t: Thread) (icon: .inbox, label: "Bring back") {
    t.snoozed_until = Time("1970-01-01T00:00:00Z")
}

/// Gives the conversation a label, or takes one away: `on` says which. The label joins the
/// person's labels the first time it is given, so it has a filter of its own from then on.
action label(t: Thread, name: Text, on: Bool = not t.labels.any(l -> l == name)) (icon: .tag) {
    require name != "" "A label needs a name"
    let m = root Mail
    if on { t.labels.add(name) } else { t.labels.remove(name) }
    if on and not m.labels.any(l -> l.name == name) { m.labels.insert(Label(name: name)) }
}

/// Renames one of the person's labels, all the conversations that carry it with it.
action rename_label(m: Mail, l: Label, name: Text) {
    require name != "" "A label needs a name"
    require not m.labels.any(x -> x.name == name) "You have a label {name}"
    for a in m.accounts {
        for t in a.threads {
            if t.labels.any(x -> x == l.name) {
                t.labels.remove(l.name)
                t.labels.add(name)
            }
        }
    }
    l.name = name
}

/// Deletes one of the person's labels, and takes it off every conversation that carries it.
destructive action delete_label(m: Mail, l: Label) (icon: .trash) {
    for a in m.accounts {
        for t in a.threads {
            if t.labels.any(x -> x == l.name) { t.labels.remove(l.name) }
        }
    }
    m.labels.remove(l)
}

/// Brings a message back to the outbox: it was sent to go at a moment still to come, and it is
/// a draft again, whatever the moment.
action cancel_later(d: Draft) (icon: .undo_2, label: "Cancel scheduled send") {
    require d.sent and not d.recalled "It is not waiting to go"
    require delivery(d)?.state != "sent" "It went already"
    withdraw(d, say("Scheduled send cancelled"))
    d.sent = false
    d.recalled = true
}

/// Sends the message at a moment instead of now: it waits in the outbox until then, as the
/// person's hold makes a message wait, and goes with no window open. Until it goes it can be
/// taken back, and bringing it back leaves it a draft.
action send_later(d: Draft, at: Time) uses time (icon: .clock, label: "Send later") {
    require not d.sent or d.recalled "This message went already"
    require d.to != "" "It goes to nobody yet"
    require at > now "Pick a moment still to come"
    outside(
        "smtp.send",
        (d.from, d.to, d.cc, d.subject, d.body, d.in_reply_to, d.references),
        at: at,
    )
    d.send_at = at
    d.sent = true
    d.recalled = false
}

/// Sends the message at nine tomorrow, the moment people reach for most when they write in the
/// evening and do not want it to arrive then.
action send_in_the_morning(d: Draft) uses time (icon: .clock, label: "Send tomorrow morning") {
    send_later(d, Time.parse("tomorrow 9am", near: now) ?? now + 18h)
}

/// Marks every conversation of the account read.
action mark_all_read(a: Account) (icon: .mail_open, label: "Mark all read") {
    for t in a.threads {
        mark_read(t, false)
    }
}

/// Mutes the conversation, or unmutes it: a muted one stays out of the inbox until it is
/// brought back, whatever its new messages do.
action mute(t: Thread, muted: Bool = not t.muted) (
    label: if t.muted { "Unmute" } else { "Mute" },
    icon: .bell_off,
    on: t.muted,
) {
    t.muted = muted
}

/// Marks the conversation read, or unread again.
action mark_read(t: Thread, unread: Bool = not t.unread) (
    label: if t.unread { "Mark as read" } else { "Mark as unread" },
    icon: .mail_open,
    on: not t.unread,
    key: "u",
) {
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

/// Sends the message from another of the person's accounts.
action send_from(d: Draft, a: Account) (icon: .user) {
    require not d.sent or d.recalled "This message went already"
    d.from = a.key
}

/// Throws a message being written away.
destructive action discard(m: Mail, d: Draft) (icon: .trash, label: "Delete draft") {
    m.drafts.remove(d)
}

/// Sends the message from its account. It leaves the system, so it goes once, and waits in the
/// outbox until the server took it; it is held back for the person's hold first, so they can
/// take it back. The effect is an argument the action records, so the wait survives a window
/// closing and an agent takes it back the same way.
primary action send(d: Draft) uses time proposal compose(d) (icon: .send) {
    require not d.sent or d.recalled "This message went already"
    require d.to != "" "It goes to nobody yet"
    let m = root Mail
    let at = now + Duration(m.hold)
    outside(
        "smtp.send",
        (d.from, d.to, d.cc, d.subject, d.body, d.in_reply_to, d.references),
        at: at,
    )
    d.send_at = at
    d.sent = true
    d.recalled = false
}

/// Takes back a message that was sent and has not gone yet: it never leaves, and the message is
/// a draft again, as it was. Once it went it cannot be taken back.
action undo_send(d: Draft) (icon: .undo_2, label: "Undo send") {
    require d.sent and not d.recalled "It has not been sent"
    require delivery(d)?.state != "sent" "It went already"
    withdraw(d, say("Taken back before it went"))
    d.sent = false
    d.recalled = true
}

/// Answers the conversation with what was typed: the message goes to whoever wrote its newest,
/// as a reply to it, quoted.
action answer(t: Thread, body: Text) -> Draft uses time (icon: .reply) {
    require body.len() > 0 "Say something first"
    let m = root Mail
    let last = t.messages.last()
    let said = if let l = last { quoted(l) } else { "" }
    let signature = signature_of(m, t.account)
    let lead = if signature == "" { body } else { "{body}\n{signature}" }
    let d = m.drafts.insert(
        Draft(
            from: t.account,
            to: last?.from ?? "",
            subject: re(t.subject),
            body: "{lead}{said}",
            in_reply_to: last?.message_id ?? "",
            references: "{last?.references ?? ""} {last?.message_id ?? ""}",
        ),
    )
    send(d)
    d
}

/// The message as it will leave, shown when someone is asked to send it.
view compose(d: Draft) {
    stack(gap: .related) {
        text("To {d.to}", role: .title)
        text(d.subject, role: .caption)
        text(d.body)
    }
}

/// The query a label's place has: the conversations that carry it, whatever mailbox they are
/// in.
fn label_query(name: Text) -> Text {
    "labels:\"{name}\""
}

/// What the window calls the place `query` shows: the filters' and the labels' names, and
/// "All mail" for every conversation.
fn place_title(m: Mail, query: Text) -> Text {
    // No query is the window on every conversation: the place the sidebar calls All mail.
    if query.is_empty() {
        say("All mail")
    } else {
        filters(Thread).find(f -> f.query == query)?.label
            ?? m.labels.find(l -> label_query(l.name) == query)?.name
            ?? say("All mail")
    }
}

/// What the account's server says of itself, over the mail it holds: the notice in the list's
/// own gutter, so its rounded card stands where the rows' boxes do. An account that stands well
/// says nothing and takes no room, so the list stands at the column's edge.
view account_notice(a: Account) {
    if a.trouble != "" {
        stack(padding: .group) {
            std.service_banner(
                a.address,
                a.online,
                a.trouble,
                retry: check_now(a),
                fetched: fetched_clock(a),
            )
        }
    }
}

/// The mail window: every account's mail. The unified inbox is every account's inbox in one
/// list, a conversation in any account opening beside it; one account's window is its
/// mailboxes as its places.
view mail(m: Mail) {
    state account: Text = ""
    if m.accounts.is_empty() {
        std.page(m) {
            ui.EmptyState("Add a mail account in Settings", icon: .mail)
        }
    } else if let a = m.accounts.find(x -> x.key == account) {
        mailboxes(a)
    } else {
        unified(m)
    }
}

/// Every account's mail in one window: the unified inbox first, then each account's mail, and
/// each account's own places under it. A conversation shows which account it came to.
view unified(m: Mail) {
    state query: Text = "is:inbox"
    let places = filters(Thread).filter(f -> f.toggle)
    let title = place_title(m, query)
    if let a = m.accounts.first() {
        let shown = a.threads.matching(query)
        ui.NavShell(
            content_kind: .document,
            sidebar_title: "All accounts",
            overview_title: title,
            overview_count: shown.len(),
        ) {
            sidebar_actions: {
                // The accounts whose server does not stand well, beside the name of the accounts
                // they are: each mark names its own account on hover, so none is passed over.
                for acct in m.accounts {
                    std.service_mark(acct.address, acct.online, acct.trouble)
                }
            }
            sidebar: {
                ui.NavList(selected: query) {
                    ui.NavItem("is:inbox", "All inboxes", icon: .inbox) {
                        on select: set(query, "is:inbox")
                    }
                    ui.NavItem("all", "All mail", icon: .mail) {
                        on select: set(query, "")
                    }
                    if not m.labels.is_empty() {
                        ui.NavSection("Labels") {
                            for l in m.labels {
                                ui.NavItem(
                                    label_query(l.name),
                                    l.name,
                                    icon: .tag,
                                    count: a.threads.matching(label_query(l.name)).len(),
                                ) {
                                    on select: set(query, label_query(l.name))
                                }
                            }
                        }
                    }
                    for acct in m.accounts {
                        ui.NavSection(acct.name) {
                            for p in places {
                                ui.NavItem("{acct.key}/{p.query}", p.label, icon: p.icon) {
                                    on select: open(acct)
                                }
                            }
                        }
                    }
                }
            }
            overview: {
                // The list shows `a`'s mail, so what the account's server says of itself is
                // said over it, and `Try again` asks the server that account reads from. The
                // notice keeps its own gutter, so the list stands under it with none between.
                stack(fill, gap: .none) {
                    account_notice(a)
                    thread_list(shown, fill)
                }
            }
        }
    }
}

/// An account's window: its mailboxes in the sidebar with how many unread each holds, the
/// conversations of the one chosen, newest first, and the one open beside them. Opening a
/// conversation reads it. The mailbox is the window's, as people type it in search
/// (`is:inbox`), so everyone looking sees the same list; it starts on the inbox. The person's
/// labels come after the mailboxes, each showing the conversations it is on.
///
/// The account's server is the window's too: a mark beside the account's address says while it
/// cannot be reached or what it fetched could not be saved, and the same is said over its mail,
/// where the person reads it.
view mailboxes(a: Account) {
    state query: Text = "is:inbox"
    let m = root Mail
    let places = filters(Thread).filter(f -> f.toggle)
    let shown = a.threads.matching(query)
    let title = place_title(m, query)
    ui.NavShell(
        content_kind: .document,
        sidebar_title: a.address,
        overview_title: title,
        overview_count: shown.len(),
    ) {
        sidebar_actions: {
            std.service_mark(a.address, a.online, a.trouble)
        }
        sidebar: {
            ui.NavList(selected: if query == "" { "all" } else { query }) {
                for p in places {
                    let unread = a.threads.matching("{p.query} is:unread").len()
                    ui.NavItem(p.query, p.label, icon: p.icon, count: if unread > 0 { unread }) {
                        on select: set(query, p.query)
                    }
                }
                ui.NavItem("all", "All mail", icon: .mail) {
                    on select: set(query, "")
                }
                if not m.labels.is_empty() {
                    ui.NavSection("Labels") {
                        for l in m.labels {
                            ui.NavItem(
                                label_query(l.name),
                                l.name,
                                icon: .tag,
                                count: a.threads.matching(label_query(l.name)).len(),
                            ) {
                                on select: set(query, label_query(l.name))
                            }
                        }
                    }
                }
            }
        }
        overview: {
            stack(fill, gap: .none) {
                account_notice(a)
                thread_list(shown, fill)
            }
        }
    }
}

/// A mailbox on its own, as a place shows it: the conversations in it, newest first, as its
/// account's window lists them under it. The inbox leaves out what is snoozed or muted, as
/// its place in the window does, and it says what its account's server does over them.
view inbox(b: Mailbox) {
    let m = root Mail
    if let a = m.accounts.find(x -> x.mailboxes.any(y -> y.id() == b.id())) {
        stack(fill, gap: .none) {
            account_notice(a)
            thread_list(in_mailbox(a, b), fill)
        }
    } else {
        ui.EmptyState("Nothing here", icon: .inbox)
    }
}

/// The conversations `b`, a mailbox of `a`'s, holds: those its filter finds, as people type
/// it in search (`is:inbox`), or those filed in it when no filter names it.
fn in_mailbox(a: Account, b: Mailbox) -> List<Thread> {
    let query = "is:{b.key}"
    if filters(Thread).any(f -> f.query == query) {
        a.threads.matching(query)
    } else {
        a.threads.filter(t -> t.boxes.any(x -> x == b.key))
    }
}

/// Conversations as a list: each a row that opens the conversation beside it, read as it
/// opens, its actions on its menu and the destructive one on its swipe; "Nothing here" when
/// there are none.
view thread_list(shown: List<Thread>) {
    if shown.is_empty() {
        ui.EmptyState("Nothing here", icon: .inbox)
    } else {
        ui.VirtualList(selection_follows_focus: true, in_column: true) {
            on select(x: Thread): open(x, front: false).then(mark_read(x, false))
            on activate(x: Thread): open(x).then(mark_read(x, false))
            for x in shown {
                ui.ListItem(item: x, presence: lenses(x).find(l -> not l.mine)?.who) {
                    thread_row(x)
                }
            }
        }
    }
}

/// One conversation in a list: who wrote, the subject, the start of the newest message and
/// when it came.
view thread_row(t: Thread) {
    row(
        t.senders,
        secondary: t.subject,
        meta: t.latest,
        mark: if t.flagged { .flag } else if t.unread { .mail },
    )
}

/// One message being written in a list: its subject and who it goes to.
view draft_row(d: Draft) {
    row(
        meta(d).title,
        secondary: if d.sent or d.recalled { delivery_says(d) } else { d.to },
        mark: if d.sent and not d.recalled { .send },
    )
}

/// How a message stands: sent, sending, waiting to send or to go online (and why), not sent and
/// why, taken back, or not known.
fn delivery_says(d: Draft) -> Text {
    if d.recalled {
        say("Taken back, and a draft again")
    } else if let s = delivery(d) {
        if s.state == "sent" {
            say("Sent")
        } else if s.state == "sending" {
            say("Sending")
        } else if s.state == "rejected" {
            say("Not sent: {s.reason}")
        } else if s.state == "unknown" {
            say("Not known whether it was sent")
        } else if s.state == "held" {
            say("Sending in a moment")
        } else if s.reason == "" {
            say("Waiting to send")
        } else { say("Waiting to go online: {s.reason}") }
    } else { "" }
}

/// Whether a message was sent and can still be taken back: nothing carried it out yet.
fn can_undo(d: Draft) -> Bool {
    d.sent and not d.recalled and delivery(d)?.state != "sent"
}

/// A conversation's window: its subject, then every message in order with who wrote it, to
/// whom and when, what it says and what came with it, and a reply at the foot.
view conversation(t: Thread) {
    state draft: Text
    ui.NavShell(content_kind: .page, title: t.subject) {
        content: {
            ui.Docked {
                foot: {
                    row {
                        ui.TextInput(bind: draft, placeholder: "Reply to {t.subject}", fill) {
                            on submit: answer(t, draft).then(set(draft, ""))
                        }
                        ui.Button("Reply", icon: .send, icon_only: true) {
                            on press: answer(t, draft).then(set(draft, ""))
                        }
                    }
                }
                ui.VirtualList(page_column: true, at_measure: true) {
                    for m in t.messages {
                        ui.ListItem(item: m) {
                            message(m)
                        }
                    }
                }
            }
        }
    }
}

/// One message of a conversation: who wrote it, to whom, what it says and, folded under it,
/// the message it quotes, as Gmail and Apple Mail fold a quotation; and what came with it. The
/// hover toolbar and the menu are the list item's: wherever the message's layout places them,
/// the list runs them.
view message(m: Message) {
    state quoting: Bool
    let body = as_lines(m.body)
    let at = quote_begins(body)
    let said = if let i = at { body.slice(0, i) } else { body }
    let quoted_part = if let i = at { body.slice(i + 2, body.len()) } else { "" }
    // The card of what the message says stands a section's gap under the line naming who wrote
    // it: close enough to belong to it, far enough to be a card of its own.
    stack(gap: .section) {
        row(gap: .related, align: .center) {
            ui.Text(if m.from_name == "" { m.from } else { m.from_name }, role: .subheading)
            ui.Time(m.received, role: .caption, past: true, secondary: true)
            spacer()
            ui.HoverToolbar {
                ui.Button("Reply", icon: .reply, icon_only: true) {
                    on press: reply_to(m)
                }
                ui.Button("Reply all", icon: .reply_all, icon_only: true) {
                    on press: reply_all_to(m)
                }
                ui.Button("Forward", icon: .forward, icon_only: true) {
                    on press: forward_message(m)
                }
            }
        }
        ui.ContextMenu {
            ui.MenuItem("Reply", icon: .reply) {
                on choose: reply_to(m)
            }
            ui.MenuItem("Reply all", icon: .reply_all) {
                on choose: reply_all_to(m)
            }
            ui.MenuItem("Forward", icon: .forward) {
                on choose: forward_message(m)
            }
        }
        ui.Card {
            ui.Text("to {m.to}", role: .caption, secondary: true)
            ui.Text(said, role: .body)
            if at != .none {
                ui.DisclosureRow(
                    if quoting { "Hide quoted text" } else { "Show quoted text" },
                    icon: .quote,
                ) {
                    on toggle: set(quoting, not quoting)
                    if quoting {
                        ui.Text(quoted_part, role: .body, secondary: true)
                    }
                }
            }
            if m.attachments != "" {
                ui.Text("Attached: {m.attachments}", role: .label)
            }
        }
    }
}

/// A message being written: its window.
view editor(d: Draft) {
    std.document(d) {
        if d.sent or d.recalled {
            field("Status") {
                text(delivery_says(d))
            }
        }
        if can_undo(d) {
            ui.Banner(
                "Sending",
                message: "It goes in a moment.",
                kind: .info,
                action: "Undo send",
            ) {
                on act: undo_send(d)
            }
        }
        field("From") {
            let mail = root Mail
            if mail.accounts.len() > 1 {
                row(gap: .tight, wrap: true) {
                    for a in mail.accounts {
                        ui.Chip(a.address, selected: a.key == d.from) {
                            on press: send_from(d, a)
                        }
                    }
                }
            } else if let a = mail.accounts.first() {
                text(a.address)
            }
        }
        field("To") {
            input(bind: d.to, placeholder: "name@example.org")
        }
        field("Cc") {
            input(bind: d.cc)
        }
        input(bind: d.subject, placeholder: "Subject")
        ui.BlockEditor(bind: d.body, placeholder: "Write your message")
    }
}

/// An inbox a working week in: conversations to read, some answered, one flagged, a few
/// archived, sent and junk.
scenario "a busy inbox" -> Thread {
    let m = root Mail
    let a = m.accounts.insert(
        Account(
            key: "sam@rivera.example",
            address: "sam@rivera.example",
            name: "Sam Rivera",
            online: true,
        ),
    )
    a.mailboxes.insert(Mailbox(key: "inbox", name: "Inbox", role: "inbox", unread: 3, total: 7))
    a.mailboxes.insert(Mailbox(key: "archive", name: "Archive", role: "archive", total: 2))
    a.mailboxes.insert(Mailbox(key: "sent", name: "Sent", role: "sent", total: 1))
    a.mailboxes.insert(Mailbox(key: "drafts", name: "Drafts", role: "drafts"))
    a.mailboxes.insert(Mailbox(key: "junk", name: "Junk", role: "junk", total: 1))
    a.mailboxes.insert(Mailbox(key: "trash", name: "Trash", role: "trash"))
    let launch = a.threads.insert(
        Thread(
            key: "launch@rivera.example",
            account: a.key,
            subject: "Launch plan for the autumn release",
            latest: 2026-06-30T10:34,
            snippet: "Agreed on the twelfth. I will tell support today so they can prepare the help pages.",
            senders: "Ada Lovelace, Grace Hopper",
            count: 3,
            unread: true,
        ),
    )
    launch.boxes.add("inbox")
    launch.messages.insert(
        Message(
            key: "launch@rivera.example",
            account: a.key,
            subject: "Launch plan for the autumn release",
            from: "grace@hopper.example",
            from_name: "Grace Hopper",
            to: "sam@rivera.example, ada@analytical.example",
            received: 2026-06-30T08:20,
            snippet: "Here is the draft plan.",
            body: "Hi both,\n\nHere is the draft plan for the autumn release. The beta goes out on the fifth, the release on the twelfth if nothing turns up.\n\nCould you each look at your part by Wednesday?\n\nGrace",
            message_id: "launch@rivera.example",
            mailbox: "inbox",
        ),
    )
    launch.messages.insert(
        Message(
            key: "launch-2@rivera.example",
            account: a.key,
            subject: "Re: Launch plan for the autumn release",
            from: "sam@rivera.example",
            from_name: "Sam Rivera",
            to: "grace@hopper.example, ada@analytical.example",
            received: 2026-06-30T09:40,
            snippet: "The fifth works for the beta.",
            body: "The fifth works for the beta. I would move the release to the fourteenth so the docs can catch up.\n\nSam",
            message_id: "launch-2@rivera.example",
            references: "launch@rivera.example",
            mailbox: "sent",
        ),
    )
    launch.messages.insert(
        Message(
            key: "launch-3@rivera.example",
            account: a.key,
            subject: "Re: Launch plan for the autumn release",
            from: "ada@analytical.example",
            from_name: "Ada Lovelace",
            to: "sam@rivera.example, grace@hopper.example",
            received: 2026-06-30T10:34,
            snippet: "Agreed on the twelfth.",
            body: "Agreed on the twelfth. I will tell support today so they can prepare the help pages.\n\nAda",
            message_id: "launch-3@rivera.example",
            references: "launch@rivera.example launch-2@rivera.example",
            unread: true,
            mailbox: "inbox",
        ),
    )
    launch.boxes.add("sent")
    let lease = a.threads.insert(
        Thread(
            key: "lease@lettings.example",
            account: a.key,
            subject: "Your lease renewal",
            latest: 2026-06-30T10:25,
            snippet: "The new lease is attached. Please sign by Friday so we can register it before the month ends.",
            senders: "Mill Lane Lettings",
            count: 1,
            unread: true,
            flagged: true,
        ),
    )
    lease.boxes.add("inbox")
    lease.messages.insert(
        Message(
            key: "lease@lettings.example",
            account: a.key,
            subject: "Your lease renewal",
            from: "office@lettings.example",
            from_name: "Mill Lane Lettings",
            to: "sam@rivera.example",
            received: 2026-06-30T10:25,
            snippet: "The new lease is attached.",
            body: "Dear Sam,\n\nThe new lease is attached. Please sign by Friday so we can register it before the month ends.\n\nKind regards,\nMill Lane Lettings",
            message_id: "lease@lettings.example",
            attachments: "Lease 2026.pdf",
            unread: true,
            flagged: true,
            mailbox: "inbox",
        ),
    )
    let dinner = a.threads.insert(
        Thread(
            key: "dinner@bo.example",
            account: a.key,
            subject: "Dinner on Friday?",
            latest: 2026-06-30T08:51,
            snippet: "Shall we try the new place on Mill Lane? I can book for eight.",
            senders: "Bo Chen",
            count: 1,
            unread: true,
        ),
    )
    dinner.boxes.add("inbox")
    dinner.messages.insert(
        Message(
            key: "dinner@bo.example",
            account: a.key,
            subject: "Dinner on Friday?",
            from: "bo@chen.example",
            from_name: "Bo Chen",
            to: "sam@rivera.example",
            received: 2026-06-30T08:51,
            snippet: "Shall we try the new place?",
            body: "Shall we try the new place on Mill Lane? I can book for eight.\n\nBo",
            message_id: "dinner@bo.example",
            unread: true,
            mailbox: "inbox",
        ),
    )
    let review = a.threads.insert(
        Thread(
            key: "review@code.example",
            account: a.key,
            subject: "Review requested: importer handles merged cells",
            latest: 2026-06-29T17:05,
            snippet: "Chen asked you to review the change to the table importer.",
            senders: "Forge",
            count: 1,
        ),
    )
    review.boxes.add("inbox")
    review.messages.insert(
        Message(
            key: "review@code.example",
            account: a.key,
            subject: "Review requested: importer handles merged cells",
            from: "notifications@code.example",
            from_name: "Forge",
            to: "sam@rivera.example",
            received: 2026-06-29T17:05,
            snippet: "Chen asked you to review.",
            body: "Chen asked you to review the change to the table importer: merged cells are kept, and the column widths follow the source.",
            message_id: "review@code.example",
            mailbox: "inbox",
        ),
    )
    let library = a.threads.insert(
        Thread(
            key: "library@city.example",
            account: a.key,
            subject: "Two books due next week",
            latest: 2026-06-29T09:30,
            snippet: "Piranesi and The Dispossessed are due on the seventh. You can renew them twice more.",
            senders: "City Library",
            count: 1,
        ),
    )
    library.boxes.add("inbox")
    library.messages.insert(
        Message(
            key: "library@city.example",
            account: a.key,
            subject: "Two books due next week",
            from: "loans@city.example",
            from_name: "City Library",
            to: "sam@rivera.example",
            received: 2026-06-29T09:30,
            snippet: "Two books are due.",
            body: "Piranesi and The Dispossessed are due on the seventh. You can renew them twice more.",
            message_id: "library@city.example",
            mailbox: "inbox",
        ),
    )
    let photos = a.threads.insert(
        Thread(
            key: "photos@mia.example",
            account: a.key,
            subject: "Photos from the lake",
            latest: 2026-06-28T19:44,
            snippet: "The sunset ones came out best. I put the rest in the shared album.",
            senders: "Mia Novak",
            count: 1,
        ),
    )
    photos.boxes.add("inbox")
    photos.messages.insert(
        Message(
            key: "photos@mia.example",
            account: a.key,
            subject: "Photos from the lake",
            from: "mia@novak.example",
            from_name: "Mia Novak",
            to: "sam@rivera.example",
            received: 2026-06-28T19:44,
            snippet: "The sunset ones came out best.",
            body: "The sunset ones came out best. I put the rest in the shared album.\n\nMia",
            message_id: "photos@mia.example",
            attachments: "lake-1.jpg\nlake-2.jpg",
            mailbox: "inbox",
        ),
    )
    let invoice = a.threads.insert(
        Thread(
            key: "invoice@studio.example",
            account: a.key,
            subject: "Invoice for June",
            latest: 2026-06-27T08:00,
            snippet: "Thanks for the work this month. The invoice is attached, payable within thirty days.",
            senders: "North Studio",
            count: 1,
        ),
    )
    invoice.boxes.add("archive")
    invoice.messages.insert(
        Message(
            key: "invoice@studio.example",
            account: a.key,
            subject: "Invoice for June",
            from: "accounts@studio.example",
            from_name: "North Studio",
            to: "sam@rivera.example",
            received: 2026-06-27T08:00,
            snippet: "The invoice is attached.",
            body: "Thanks for the work this month. The invoice is attached, payable within thirty days.",
            message_id: "invoice@studio.example",
            attachments: "Invoice 0612.pdf",
            mailbox: "archive",
        ),
    )
    let offer = a.threads.insert(
        Thread(
            key: "offer@deals.example",
            account: a.key,
            subject: "You have won a cruise",
            latest: 2026-06-26T03:12,
            snippet: "Claim your prize within 24 hours by confirming your card details.",
            senders: "Prize Desk",
            count: 1,
        ),
    )
    offer.boxes.add("junk")
    offer.messages.insert(
        Message(
            key: "offer@deals.example",
            account: a.key,
            subject: "You have won a cruise",
            from: "win@deals.example",
            from_name: "Prize Desk",
            to: "sam@rivera.example",
            received: 2026-06-26T03:12,
            snippet: "Claim your prize.",
            body: "Claim your prize within 24 hours by confirming your card details.",
            message_id: "offer@deals.example",
            mailbox: "junk",
        ),
    )
    launch
}

/// An answer being written to a long conversation: its window, with whom it goes to and what it
/// says so far.
scenario "a reply being written" -> Draft {
    let m = root Mail
    let a = m.accounts.insert(
        Account(
            key: "sam@rivera.example",
            address: "sam@rivera.example",
            name: "Sam Rivera",
            online: true,
        ),
    )
    a.mailboxes.insert(Mailbox(key: "inbox", name: "Inbox", role: "inbox", total: 1))
    a.mailboxes.insert(Mailbox(key: "drafts", name: "Drafts", role: "drafts"))
    m.drafts.insert(
        Draft(
            from: a.key,
            to: "grace@hopper.example, ada@analytical.example",
            cc: "support@rivera.example",
            subject: "Re: Launch plan for the autumn release",
            body: "The fifth works for the beta. I would move the release to the fourteenth so the docs can catch up.\n\nSam",
            in_reply_to: "launch@rivera.example",
            references: "launch@rivera.example",
        ),
    )
}

/// No account yet: what a first start shows.
scenario "no account yet" {
    root Mail
}

/// An account whose server cannot be reached: the mail under it is what was last fetched, and
/// the window says so beside the account and over its mail.
scenario "an account that cannot be reached" -> Account {
    let m = root Mail
    let a = m.accounts.insert(
        Account(
            key: "sam@rivera.example",
            address: "sam@rivera.example",
            name: "Sam Rivera",
            trouble: "connection refused",
            online: false,
            fetched: 2026-06-30T10:41,
        ),
    )
    a.mailboxes.insert(Mailbox(key: "inbox", name: "Inbox", role: "inbox", unread: 2, total: 3))
    a.mailboxes.insert(Mailbox(key: "archive", name: "Archive", role: "archive", total: 1))
    a.mailboxes.insert(Mailbox(key: "sent", name: "Sent", role: "sent", total: 1))
    a.mailboxes.insert(Mailbox(key: "drafts", name: "Drafts", role: "drafts"))
    let lease = a.threads.insert(
        Thread(
            key: "lease@lettings.example",
            account: a.key,
            subject: "Your lease renewal",
            latest: 2026-06-30T10:25,
            snippet: "The new lease is attached. Please sign by Friday so we can register it before the month ends.",
            senders: "Mill Lane Lettings",
            count: 1,
            unread: true,
            flagged: true,
        ),
    )
    lease.boxes.add("inbox")
    lease.messages.insert(
        Message(
            key: "lease@lettings.example",
            account: a.key,
            subject: "Your lease renewal",
            from: "office@lettings.example",
            from_name: "Mill Lane Lettings",
            to: "sam@rivera.example",
            received: 2026-06-30T10:25,
            snippet: "The new lease is attached.",
            body: "Dear Sam,\n\nThe new lease is attached. Please sign by Friday so we can register it before the month ends.\n\nKind regards,\nMill Lane Lettings",
            message_id: "lease@lettings.example",
            attachments: "Lease 2026.pdf",
            unread: true,
            flagged: true,
            mailbox: "inbox",
        ),
    )
    let dinner = a.threads.insert(
        Thread(
            key: "dinner@bo.example",
            account: a.key,
            subject: "Dinner on Friday?",
            latest: 2026-06-30T08:51,
            snippet: "Shall we try the new place on Mill Lane? I can book for eight.",
            senders: "Bo Chen",
            count: 1,
            unread: true,
        ),
    )
    dinner.boxes.add("inbox")
    dinner.messages.insert(
        Message(
            key: "dinner@bo.example",
            account: a.key,
            subject: "Dinner on Friday?",
            from: "bo@chen.example",
            from_name: "Bo Chen",
            to: "sam@rivera.example",
            received: 2026-06-30T08:51,
            snippet: "Shall we try the new place?",
            body: "Shall we try the new place on Mill Lane? I can book for eight.\n\nBo",
            message_id: "dinner@bo.example",
            unread: true,
            mailbox: "inbox",
        ),
    )
    a
}

/// One mailbox on its own, as a place on someone's canvas shows it: an account, its inbox and
/// the conversations in it, and an archived one it leaves out.
scenario "a mailbox on its own" -> Mailbox {
    let m = root Mail
    let a = m.accounts.insert(
        Account(
            key: "sam@rivera.example",
            address: "sam@rivera.example",
            name: "Sam Rivera",
            online: true,
        ),
    )
    let inbox = a.mailboxes.insert(
        Mailbox(key: "inbox", name: "Inbox", role: "inbox", unread: 1, total: 2),
    )
    a.mailboxes.insert(Mailbox(key: "archive", name: "Archive", role: "archive", total: 1))
    let lease = a.threads.insert(
        Thread(
            key: "lease@lettings.example",
            account: a.key,
            subject: "Your lease renewal",
            latest: 2026-06-30T10:25,
            snippet: "The new lease is attached. Please sign by Friday so we can register it before the month ends.",
            senders: "Mill Lane Lettings",
            count: 1,
            unread: true,
            flagged: true,
        ),
    )
    lease.boxes.add("inbox")
    lease.messages.insert(
        Message(
            key: "lease@lettings.example",
            account: a.key,
            subject: "Your lease renewal",
            from: "office@lettings.example",
            from_name: "Mill Lane Lettings",
            to: "sam@rivera.example",
            received: 2026-06-30T10:25,
            snippet: "The new lease is attached.",
            body: "Dear Sam,\n\nThe new lease is attached. Please sign by Friday so we can register it before the month ends.\n\nKind regards,\nMill Lane Lettings",
            message_id: "lease@lettings.example",
            unread: true,
            flagged: true,
            mailbox: "inbox",
        ),
    )
    let invoice = a.threads.insert(
        Thread(
            key: "invoice@studio.example",
            account: a.key,
            subject: "Invoice for June",
            latest: 2026-06-27T08:00,
            snippet: "Thanks for the work this month. The invoice is attached, payable within thirty days.",
            senders: "North Studio",
            count: 1,
        ),
    )
    invoice.boxes.add("archive")
    invoice.messages.insert(
        Message(
            key: "invoice@studio.example",
            account: a.key,
            subject: "Invoice for June",
            from: "accounts@studio.example",
            from_name: "North Studio",
            to: "sam@rivera.example",
            received: 2026-06-27T08:00,
            snippet: "The invoice is attached.",
            body: "Thanks for the work this month. The invoice is attached, payable within thirty days.",
            message_id: "invoice@studio.example",
            attachments: "Invoice 0612.pdf",
            mailbox: "archive",
        ),
    )
    inbox
}
