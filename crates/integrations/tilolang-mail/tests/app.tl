app so.tilo.mail.test "Mail tests"

// The app's own types and its send, kept to what a test reads and drives: an account, its
// conversations and their messages as its server holds them, the drafts a person writes, and
// the one way a message leaves.

/// What the server of the workspace's mail account holds. The account's address, servers and
/// signature are the workspace settings', and its credentials stay in ownership.
type Account (origin: imap.account) {
    key: Text merge register
    address: Text
    name: Text
    /// What the account's server said when it could not be reached, in its words.
    trouble: Text
    /// Of what kind that trouble is: `signed-out`, `no-password`, `withheld`, `offline` or
    /// `unreachable`. Empty while the account stands well.
    trouble_kind: Text = ""
    online: Bool
    fetched: Time = Time("1970-01-01T00:00:00Z")
    identities: Text
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
    more: Bool
}

/// A conversation: messages that answer each other, newest last.
type Thread (origin: imap.thread, title: subject) {
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
    arrived: Option<Time> = .none
}

/// A message.
type Message (origin: imap.message, title: subject) {
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
    unread: Bool
    flagged: Bool
    mailbox: Text
}

/// A message being written: who it goes to, what it says, and whether it went.
type Draft (title: subject) {
    from: Text
    from_address: Text = ""
    to: Text
    cc: Text
    bcc: Text
    subject: Text
    body: Text
    in_reply_to: Text
    references: Text
    sent: Bool = false
}

/// The workspace mailbox holds its accounts and the drafts being written.
type Mail {
    accounts: Collection<Account>
    drafts: Collection<Draft>
}

root mail: Mail

default view main for Mail

default view thread_view for Thread

default view draft_view for Draft

view main(m: Mail) {
    page {
        for a in m.accounts {
            text(a.key)
        }
        for d in m.drafts {
            text(d.subject)
        }
    }
}

view thread_view(t: Thread) {
    page {
        text(t.subject)
        for message in t.messages {
            text(message.subject)
        }
    }
}

view draft_view(d: Draft) {
    page {
        text(d.to)
        text(d.subject)
    }
}

/// Writes a message to go from the account: what a person starting one does, with whom it goes
/// to, its subject and what it says.
action compose(m: Mail, account: Text, to: Text, subject: Text, body: Text) -> Draft {
    require has_account(m, account) "Add a mail account in Settings first"
    m.drafts.insert(Draft(from: account, to: to, subject: subject, body: body))
}

fn has_account(m: Mail, key: Text) -> Bool {
    m.accounts.any(a -> a.key == key)
}

/// Sends the message: it leaves through the account's server, and waits in the outbox until
/// the server took it.
action send(d: Draft) {
    require not d.sent "This message went already"
    require d.to != "" or d.bcc != "" "It goes to nobody yet"
    require d.from != "" "Pick the account it goes from"
    require has_account(root Mail, d.from) "Add a mail account in Settings first"
    outside(
        "smtp.send",
        (
            d.from,
            d.from_address,
            d.to,
            d.cc,
            d.bcc,
            d.subject,
            d.body,
            d.in_reply_to,
            d.references,
        ),
    )
    d.sent = true
}

/// Asks the account's server for its mail now, and says how it answered on the account.
action check(a: Account) {
    outside("imap.check", a.key)
}
