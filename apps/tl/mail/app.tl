app so.tilo.mail "Mail" (
    icon: .mail,
    category: .office,
    tagline: "Email over IMAP and SMTP",
    keywords: ["email", "e-mail", "mail", "inbox", "mailbox", "gmail", "imap", "smtp", "apple mail", "evolution", "geary", "kmail", "kontact", "emails", "post", "posteingang", "briefe", "postfach", "mailprogramm", "elektronische post", "e-mails"],
)

use so.tilo.contacts as contacts

/// What the server of one of the workspace's mail accounts holds. The account itself, its
/// address, servers and signature, is the workspace settings', and its credentials stay in
/// ownership; the system keeps this in step with the server.
type Account (origin: imap.account) {
    key: Text merge register
    address: Text
    name: Text
    /// What the account's server said when it could not be reached, in its words: empty while
    /// it stands well.
    trouble: Text
    /// Of what kind that trouble is, so the window says what helps: `signed-out` (the server
    /// refused the password), `no-password` (the account has none on this device), `withheld`
    /// (the workspace's rules kept it back), `offline` (the device has no network), or
    /// `unreachable` for anything else. Empty while the account stands well.
    trouble_kind: Text = ""
    online: Bool
    /// When the account's mail was last taken in from its server, as the clock the person read
    /// it at: 1970 when it never was.
    fetched: Time = Time("1970-01-01T00:00:00Z")
    /// What the account's messages end with. Empty for none.
    signature: Text
    /// The other names and addresses the account sends as, beside its own, one `Name <address>`
    /// a line: what a person writes from when they write as a role rather than as themselves.
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
    /// Whether the account's server holds messages older than the ones this workspace holds:
    /// what "Show older messages" reads the page before them for.
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
    /// The labels members assigned to this conversation in the workspace.
    labels: Set<Text>
    /// Snoozed until this moment: out of the inbox until then, and back once it has come, with
    /// no window open. The epoch when it is not snoozed.
    snoozed_until: Time = Time("1970-01-01T00:00:00Z")
    /// Muted: the conversation stays out of the inbox while new messages keep coming, as a
    /// busy conversation is silenced, and it is still found under Muted and All mail.
    muted: Bool = false
    /// When mail last came to this conversation's inbox, while the people who read it have not
    /// been told of it: the moment the announcement runs for, and nothing when there is none.
    /// The system writes it as the mail comes and the announcement clears it.
    arrived: Option<Time> = .none

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
        // When the conversation last came: `latest:>2026-06-01`, `latest:-7d` narrow the mail a
        // person looks through to a day or a span, as every other field a filter compares.
        filter latest "Received" (icon: .clock)
        sort latest newest
        hide key
        hide account
        hide boxes
        hide arrived
    }
}

/// A workspace label for conversations. A conversation holds each label by name.
type Label {
    name: Text

    search {
        sort name a_z
    }
}

/// A message.
type Message (origin: imap.message, title: subject, incoming: from) {
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
    /// What it says as its sender formatted it: its HTML part, the pictures it shows in place read
    /// from this device's content. Empty for a message of plain text.
    html: Text
    message_id: Text merge register
    references: Text merge register
    /// The files it came with, each kept in the workspace's content: one whose bytes are not on
    /// this device yet is named with no content.
    attachments: List<File>
    unread: Bool
    flagged: Bool
    mailbox: Text
    /// The content reference of the message as it was sent, for one taken in from outside the
    /// account's server (`deliver_message`): empty for one its server holds.
    original: Text = ""
    /// Where each large part of a message taken in from outside stands in `original` and the
    /// pieces of content it is in, one JSON text a line: empty for one its server holds.
    parts: Text = ""
}

/// A message being written: who it goes to, what it says, what it answers, and how it stands on
/// its way out. The account it goes from is named by its key.
type Draft (title: subject, untitled: "New message", readers: author until sent) {
    from: Text
    /// The identity of `from` it goes from, one of the account's own (`Name <address>`): empty
    /// for the account's own address, and for one it no longer sends as.
    from_address: Text = ""
    to: Text
    cc: Text
    /// Blind recipients, who see the message but are not named in it.
    bcc: Text
    subject: Text
    body: Text
    in_reply_to: Text
    references: Text
    /// The conversation it answers, by its key: empty for a message that answers none. The
    /// draft shows it beside what is written, so the person reads what they answer.
    conversation: Text = ""
    sent: Bool = false
    /// When it goes: 1970 for a draft not sent yet.
    send_at: Time = Time("1970-01-01T00:00:00Z")
    /// It was taken back before it went: it never leaves, and it is a draft again.
    recalled: Bool = false
    /// The calendar invitation it carries (iCalendar), which goes beside what it says so the
    /// guest's calendar can answer it: empty for none.
    invitation: Text = ""
    /// The files it takes along.
    attachments: List<File>
    /// What the native Mail kept on this draft that this one has no place for (attachments,
    /// private notes), said in words: never sent with it.
    set_aside: Text

    search {
        filter "Drafts" = not sent (icon: .pencil)
        filter "Outbox" = sent and not recalled (icon: .send)
        hide in_reply_to
        hide references
        hide conversation
        hide send_at
        hide invitation
    }
}

/// The workspace mailbox holds its accounts, drafts, and conversation labels.
type Mail {
    accounts: Collection<Account>
    drafts: Collection<Draft>
    labels: Collection<Label>
    /// Conversations kept outside the IMAP account collections: messages delivered from outside
    /// an account's server, and the mail of an account that is gone.
    recovered_mail: Collection<Thread>
    /// How many seconds members have to undo a send. The default is ten seconds.
    hold: Int = 10

    search {
        filter hold
    }
}

root mail: Mail

default view mail for Mail

default view conversation for Thread

default view mailboxes for Account

default view inbox for Mailbox

place view all_inboxes for Mail

default view editor for Draft

row view thread_row for Thread

row view draft_row for Draft

/// The signature the account `key` signs its messages with: its own, and empty when it has
/// none or the workspace has no account with that key.
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

/// The account an answer to mail that came to the account `key`, or mail passed on from it,
/// goes from: that account while the workspace has it, and the workspace's first otherwise, as
/// mail kept from an account that went is answered from the one the workspace reads now. Empty
/// while the workspace has no account.
fn sends_from(m: Mail, key: Text) -> Text {
    if has_account(m, key) { key } else { m.accounts.first()?.key ?? "" }
}

/// Whether the workspace has the account `key`.
fn has_account(m: Mail, key: Text) -> Bool {
    m.accounts.any(a -> a.key == key)
}

/// Sets or clears the workspace account's signature.
action set_signature(a: Account, signature: Text) (icon: .pen_tool, label: "Signature") {
    a.signature = signature
}

/// Sets how many seconds a sent message waits before it goes, so it can be taken back: no wait
/// at all at zero.
action set_hold(m: Mail, seconds: Int) (icon: .clock, label: "Undo send time") {
    require seconds >= 0 "A message cannot wait less than no time"
    m.hold = seconds
}

/// Asks the account's server for its mail now: what a person presses when the window says the
/// server could not be reached, or that what it fetched could not be saved. The account says
/// how it stands once it answers.
action check_now(a: Account) (icon: .refresh, label: "Check now") {
    outside("imap.check", a.key)
}

/// Asks the account's server for the messages older than the oldest this workspace holds in the
/// mailbox, so a folder longer than one page can be read back to its first message. They come in
/// as new mail does, and the mailbox stops offering them once its first message is here. A
/// mailbox whose server holds nothing older answers nothing, as a list reads to its end asking
/// whether a page is there.
action older_messages(b: Mailbox) (icon: .arrow_down, label: "Show older messages") {
    if b.more {
        let m = root Mail
        if let a = m.accounts.find(x -> x.mailboxes.any(y -> y.id() == b.id())) {
            outside("imap.older", (a.key, b.key))
        }
    }
}

/// Asks the account's server to search its mail for `text`: what finds mail the workspace does
/// not hold, and mail in folders it does not sync. What it finds comes in as the account's
/// conversations, as a fetch's mail does, so it is read and searched here from then on.
action search_server(a: Account, text: Text) (icon: .search, label: "Search the server") {
    require text.trim() != "" "Say what to look for"
    outside("imap.search", (a.key, text))
}

/// Starts a message from the account.
primary action new_message(a: Account) -> Draft (icon: .pencil, label: "New message") {
    let m = root Mail
    m.drafts.insert(Draft(from: a.key, body: signature_of(m, a.key)))
}

/// The mailbox the native Mail kept before this app took its id over, carried into this one
/// once: every draft it was writing and every message still waiting in its outbox is a draft
/// here, from the same account, as the same identity, to the same people (the blind ones too),
/// with the same subject, body and invitation. A message that was waiting to go is a draft
/// again, to be sent from here. What a draft held that this app has no place for is kept on it
/// in `set_aside`, never sent. The native app's own documents are left as they are.
migration action carry_native_mail(m: Mail) {
    let earlier = earlier(m)
    // An outbox entry is waiting while it has not settled; one that went or failed for good is
    // the server's to show, not a draft.
    let waiting = earlier.filter(
        e -> e.of == "outbox"
            and e.texts.get("settled") == .none
            and e.lists.get("settled") == .none,
    )
    let carried = [
        earlier.filter(e -> e.of == "draft").flat_map(e -> native_draft(e, "", "", "")),
        earlier.filter(e -> e.of == "drafts").flat_map(e -> native_draft(e, "", "", "")),
        waiting.flat_map(
            e -> native_draft(
                e,
                "value.draft.",
                e.texts.get("value.account") ?? "",
                e.texts.get("value.in_reply_to") ?? "",
            ),
        ),
    ]
        .flat_map(x -> x)
    for draft in carried {
        m.drafts.insert(draft)
    }
}

/// The words the native record `e` kept in its field `name`; nothing when it kept none.
fn native_said(e: host.Earlier, name: Text) -> Text {
    e.texts.get(name) ?? ""
}

/// The draft the native record `e` kept under the fields named `at…`, sent from `account` and
/// answering `answers` when the draft itself names neither: none for a draft with nothing
/// written in it, which the native Mail left out too.
fn native_draft(e: host.Earlier, at: Text, account: Text, answers: Text) -> List<Draft> {
    let to = native_said(e, "{at}to")
    let cc = native_said(e, "{at}cc")
    let bcc = native_said(e, "{at}bcc")
    let subject = native_said(e, "{at}subject")
    let plain = native_said(e, "{at}body")
    let body = if plain == "" {
        (e.rich.get("{at}rich_body") ?? RichText("")).markdown
    } else { plain }
    let files = e.lists.get("{at}attachments.filename") ?? []
    let notes = native_said(e, "{at}notes")
    let sender = native_said(e, "{at}sender.account")
    let sender_address = native_said(e, "{at}sender.email")
    let reply = native_said(e, "{at}in_reply_to")
    if files.len() == 0 and not [to, cc, bcc, subject, body].any(t -> t.trim() != "") {
        []
    } else {
        let aside = [
            if files.len() == 0 {
                ""
            } else {
                "Attachments not carried over: {files.join(", ")}"
            },
            if notes == "" { "" } else { "Notes: {notes}" },
        ]
            .filter(t -> t != "")
        [
            Draft(
                from: if sender == "" { account } else { sender },
                from_address: sender_address,
                to: to,
                cc: cc,
                bcc: bcc,
                subject: subject,
                body: body,
                in_reply_to: if reply == "" { answers } else { reply },
                invitation: native_said(e, "{at}calendar_ics"),
                set_aside: aside.join("\n"),
            ),
        ]
    }
}

/// Starts a message. A message is written before the workspace has an account to send it from:
/// it waits in Drafts, and goes once the person adds one.
primary action new_draft(m: Mail) -> Draft (icon: .pencil, label: "New message") {
    let from = m.accounts.first()?.key ?? ""
    m.drafts.insert(Draft(from: from, body: signature_of(m, from)))
}

/// Writes a message for the person to read and send: to whom, its subject and what it says,
/// answering `in_reply_to` (a Message-ID) when it names one. Every call makes a draft of its own
/// beside the others; nothing is written over and nothing is sent. Where an agent drafts a
/// message, the person finds it in Drafts and sends it.
action draft_message(
    m: Mail,
    to: Text,
    subject: Text = "",
    body: Text = "",
    cc: Text = "",
    in_reply_to: Text = "",
    from: Text = "",
) -> Draft (icon: .pencil, label: "Draft a message") {
    require to != "" "It goes to nobody yet"
    let account = if from != "" and has_account(m, from) {
        from
    } else { m.accounts.first()?.key ?? "" }
    m.drafts.insert(
        Draft(
            from: account,
            to: to,
            cc: cc,
            subject: subject,
            body: body,
            in_reply_to: in_reply_to,
            references: in_reply_to,
        ),
    )
}

/// Starts a message inviting `to` to an event, its invitation (`calendar_ics`, iCalendar) carried
/// beside what it says: what Calendar hands Mail for each guest it invites. Nothing is sent; the
/// person reads it, and sends it.
action prepare_calendar_invitation(
    m: Mail,
    to: Text,
    subject: Text,
    body: Text,
    calendar_ics: Text,
) -> Draft (icon: .calendar, label: "Invite by mail") {
    require not m.accounts.is_empty() "Add a mail account in Settings first"
    require to != "" "An invitation goes to someone"
    require calendar_ics.find("BEGIN:VCALENDAR") != .none
        and calendar_ics.find("METHOD:REQUEST") != .none "The invitation is not a calendar request"
    let from = m.accounts.first()?.key ?? ""
    let signature = signature_of(m, from)
    m.drafts.insert(
        Draft(
            from: from,
            to: to,
            subject: subject,
            body: if signature == "" { body } else { "{body}\n{signature}" },
            invitation: calendar_ics,
        ),
    )
}

/// Takes in a message that reached the workspace from outside its accounts' servers: one moved
/// in from another mail program (`tilo migrate`), or handed over through the API. It is kept
/// with the mail held outside the accounts, in the conversation it belongs to (`conversation`,
/// the Message-ID the conversation began with) and the folder it was in, read or not. `from` is
/// the sender as a header writes it (`Ada Lovelace <ada@example.com>`), and `original` the
/// content reference of the message as it was sent, less its large parts: `parts` says where
/// each of those stands in it and which pieces of content it is in, one JSON text each
/// (`{"at": 120, "size": 4096, "pieces": ["tilo.content:…"]}`). `attachments` are the files it
/// carries, each content the workspace holds. A message already held under its Message-ID is
/// answered as it is, so delivering it again keeps it once.
action deliver_message(
    m: Mail,
    from: Text,
    subject: Text = "",
    body: Text = "",
    folder: Text = "inbox",
    read: Bool = false,
    to: Text = "",
    cc: Text = "",
    date: Time = Time("1970-01-01T00:00:00Z"),
    conversation: Text = "",
    message_id: Text = "",
    original: Text = "",
    parts: List<Text> = [],
    attachments: List<File> = [],
) -> Message (icon: .inbox, label: "Deliver a message") {
    require from != "" "A message says who it is from"
    if message_id != "" {
        for held in m.recovered_mail {
            if let kept = held.messages.find(x -> x.message_id == message_id) {
                return kept
            }
        }
    }
    let address = sender_address(from)
    let name = sender_name(from)
    let key = if conversation != "" {
        conversation
    } else if message_id != "" { message_id } else { "{address} {date} {subject}" }
    // What a conversation shows of itself is what its first message said: like a server's
    // conversation, it is written when it is made, and later messages are kept in it.
    let t = if let held = m.recovered_mail.find(x -> x.key == key) {
        held
    } else {
        m.recovered_mail.insert(
            Thread(
                key: key,
                subject: subject,
                latest: date,
                snippet: as_lines(body).slice(0, 200),
                senders: name,
                count: 1,
            ),
        )
    }
    t.boxes.add(folder)
    if not read {
        t.unread = true
    }
    t.messages.insert(
        Message(
            key: if message_id != "" { message_id } else { "{key} {t.messages.len()}" },
            subject: subject,
            from: address,
            from_name: name,
            to: to,
            cc: cc,
            received: date,
            snippet: as_lines(body).slice(0, 200),
            body: body,
            message_id: message_id,
            unread: not read,
            mailbox: folder,
            attachments: attachments,
            original: original,
            parts: parts.join("\n"),
        ),
    )
}

/// The address of a sender as a header writes them: what stands in `<…>`, or all of it.
fn sender_address(from: Text) -> Text {
    if let open = from.find("<") {
        let rest = from.slice(open + 1, from.len())
        if let close = rest.find(">") {
            return rest.slice(0, close).trim()
        }
    }
    from.trim()
}

/// The name of a sender as a header writes them: what stands before `<…>`, without quotes, or
/// their address when it names none.
fn sender_name(from: Text) -> Text {
    if let open = from.find("<") {
        let named = from.slice(0, open).trim().replace("\"", "")
        if named != "" {
            return named
        }
    }
    sender_address(from)
}

/// Who a header names: the person and their address, or the address alone when it is all there
/// is.
fn header_addr(name: Text, address: Text) -> Text {
    if name == "" or name == address { address } else { "{name} <{address}>" }
}

/// The name a message's export is written under: its subject with the characters a file name
/// cannot hold taken out, and "Message" for one that names nothing.
fn message_file(m: Message) -> Text {
    let safe = m.subject
        .trim()
        .replace("/", "-")
        .replace("\\", "-")
        .replace(":", "-")
        .replace("\n", " ")
        .replace("<", "-")
        .replace(">", "-")
    if safe == "" or safe == "." or safe == ".." { std.say("Message") } else { safe }
}

/// The message as a mail program writes it in one file (`.eml`): its headers, then what it says.
fn message_eml(m: Message) -> Text {
    let headers = [
        "From: {header_addr(m.from_name, m.from)}",
        if m.to == "" { "" } else { "To: {m.to}" },
        if m.cc == "" { "" } else { "Cc: {m.cc}" },
        "Subject: {m.subject}",
        "Date: {m.received.iso()}",
        if m.message_id == "" { "" } else { "Message-ID: <{m.message_id}>" },
        if m.references == "" { "" } else { "References: <{m.references}>" },
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
    ]
        .filter(line -> line != "")
    "{headers.join("\n")}\n\n{as_lines(m.body)}\n"
}

/// The message as Markdown: what it says, under who wrote it and what it is called.
fn message_markdown(m: Message) -> Text {
    let details = [
        "From: {header_addr(m.from_name, m.from)}",
        if m.to == "" { "" } else { "To: {m.to}" },
        if m.cc == "" { "" } else { "Cc: {m.cc}" },
        "Date: {m.received.iso()}",
        if m.attachments.is_empty() {
            ""
        } else {
            "Attached: {attachment_names(m).join(", ")}"
        },
    ]
        .filter(line -> line != "")
    "# {m.subject}\n\n{details.join("\n")}\n\n{as_lines(m.body)}\n"
}

/// The message as one JSON object: who wrote it, to whom, when, what it says and what came with
/// it, so a script or an agent reads it as the record it is.
fn message_json(m: Message) -> Text {
    let fields = [
        "\"from\": {json_text(m.from)}",
        "\"from_name\": {json_text(m.from_name)}",
        "\"to\": {json_text(m.to)}",
        "\"cc\": {json_text(m.cc)}",
        "\"subject\": {json_text(m.subject)}",
        "\"received\": {json_text(m.received.iso())}",
        "\"message_id\": {json_text(m.message_id)}",
        "\"references\": {json_text(m.references)}",
        "\"body\": {json_text(as_lines(m.body))}",
        "\"attachments\": {quoted_list(attachment_names(m))}",
        "\"unread\": {m.unread.to_text()}",
        "\"flagged\": {m.flagged.to_text()}",
    ]
    let written = fields.join(",\n  ")
    "\{\n  {written}\n\}\n"
}

/// The files a message carries, by name: none when it carries none.
fn attachment_names(m: Message) -> List<Text> {
    m.attachments.map(f -> f.name)
}

/// `text` as JSON writes it, quotes and escapes and all.
fn json_text(text: Text) -> Text {
    json.encode(text)
}

/// A list of texts as a JSON array.
fn quoted_list(items: List<Text>) -> Text {
    "[{items.map(item -> json_text(item)).join(", ")}]"
}

/// The message as a Markdown file: what a person exports to read it, or keep it, elsewhere.
action export_markdown(m: Message) -> File (icon: .download, label: "Export as Markdown") {
    File.of_text("{message_file(m)}.md", message_markdown(m))
}

/// The message as a JSON file: what a script or an agent reads its record from.
action export_json(m: Message) -> File (icon: .download, label: "Export as JSON") {
    File.of_text("{message_file(m)}.json", message_json(m))
}

/// The message as an `.eml` file, which every mail program opens. A message taken in from outside
/// an account's server is exported as it was sent; one its server holds is written from what the
/// workspace keeps of it.
action export_eml(m: Message) -> File (icon: .download, label: "Export as .eml") {
    if m.original.starts_with("tilo-blob://") {
        File.at("{message_file(m)}.eml", m.original)
    } else {
        File.of_text("{message_file(m)}.eml", message_eml(m))
    }
}

/// What the message file `file` holds as text, its lines ended one way: empty for a file that is
/// not a message or cannot be read.
fn message_text(file: File) -> Text {
    match file.text() {
        .ok(said) => as_lines(said),
        .err(_) => "",
    }
}

/// Where the headers of a message file end: the blank line after them, or its end when it has
/// none.
fn mail_head_at(text: Text) -> Int {
    text.find("\n\n") ?? text.len()
}

/// What a message file's headers say in the header `name`: its value, without the name and the
/// colon, and nothing when it names none.
fn header_of(text: Text, name: Text) -> Text {
    for line in text.slice(0, mail_head_at(text)).split("\n") {
        if let at = line.find(":") {
            if line.slice(0, at).trim().lower() == name {
                return line.slice(at + 1, line.len()).trim()
            }
        }
    }
    ""
}

/// Opens a message kept as a file a mail program writes (`.eml`, a single message of an mbox, or
/// Apple Mail's `.emlx`): its headers are read, and it is kept with the mail held outside the
/// accounts, read, in the inbox. The same file opened again is the message already kept.
action open_message_file(m: Mail, file: File) -> Message uses time (
    icon: .mail,
    label: "Open a message file",
    opens: ["message/rfc822", .eml, .emlx, .mbox],
) {
    require message_text(file) != "" "The file holds no message"
    let text = message_text(file)
    let head_at = mail_head_at(text)
    let message_id = header_of(text, "message-id").replace("<", "").replace(">", "").trim()
    let body = if head_at >= text.len() { "" } else { text.slice(head_at + 2, text.len()) }
    deliver_message(
        m,
        from: header_of(text, "from"),
        subject: header_of(text, "subject"),
        body: body,
        folder: "inbox",
        read: true,
        to: header_of(text, "to"),
        cc: header_of(text, "cc"),
        date: Time.parse(header_of(text, "date"), near: now) ?? now,
        conversation: message_id,
        message_id: message_id,
        original: file.url,
    )
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
/// nothing, its moment at the epoch.
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

/// The key of the conversation `m` stands in, in an account or in the mail recovered from one
/// that went: empty for a message no conversation holds.
fn conversation_of(m: Message) -> Text {
    let mail = root Mail
    for a in mail.accounts {
        if let t = a.threads.find(x -> x.messages.any(y -> y.id() == m.id())) {
            return t.key
        }
    }
    if let t = mail.recovered_mail.find(x -> x.messages.any(y -> y.id() == m.id())) {
        return t.key
    }
    ""
}

/// The conversation named by `key`, in the accounts or the mail recovered from one that went:
/// none once it is gone.
fn thread_named(m: Mail, key: Text) -> Option<Thread> {
    if key == "" {
        return .none
    }
    for a in m.accounts {
        if let t = a.threads.find(x -> x.key == key) {
            return .some(t)
        }
    }
    m.recovered_mail.find(t -> t.key == key)
}

/// Answers the conversation's newest message, to whoever wrote it, quoting it under what is
/// written.
primary action reply(t: Thread) -> Draft (icon: .reply, key: "r") {
    let m = root Mail
    let last = t.messages.last()
    let from = sends_from(m, t.account)
    m.drafts.insert(
        Draft(
            from: from,
            to: last?.from ?? "",
            subject: re(t.subject),
            body: signed(m, from, if let l = last { quoted(l) } else { "" }),
            in_reply_to: last?.message_id ?? "",
            references: "{last?.references ?? ""} {last?.message_id ?? ""}",
            conversation: t.key,
        ),
    )
}

/// Answers the conversation's newest message, to everyone it went to, quoting it.
action reply_all(t: Thread) -> Draft (icon: .reply_all, label: "Reply all", key: "a") {
    let m = root Mail
    let last = t.messages.last()
    let from = sends_from(m, t.account)
    m.drafts.insert(
        Draft(
            from: from,
            to: last?.from ?? "",
            cc: if let l = last {
                if l.cc == "" { l.to } else { "{l.to}, {l.cc}" }
            } else { "" },
            subject: re(t.subject),
            body: signed(m, from, if let l = last { quoted(l) } else { "" }),
            in_reply_to: last?.message_id ?? "",
            references: "{last?.references ?? ""} {last?.message_id ?? ""}",
            conversation: t.key,
        ),
    )
}

/// Answers one message of the conversation, to whoever wrote it, quoting it.
action reply_to(m: Message) -> Draft (icon: .reply) {
    let mail = root Mail
    let from = sends_from(mail, m.account)
    mail.drafts.insert(
        Draft(
            from: from,
            to: m.from,
            subject: re(m.subject),
            body: signed(mail, from, quoted(m)),
            in_reply_to: m.message_id,
            references: "{m.references} {m.message_id}",
            conversation: conversation_of(m),
        ),
    )
}

/// Answers one message of the conversation, to everyone it went to but the person answering.
action reply_all_to(m: Message) -> Draft (icon: .reply_all, label: "Reply all") {
    let mail = root Mail
    let theirs = if m.cc == "" { m.to } else { "{m.to}, {m.cc}" }
    let from = sends_from(mail, m.account)
    mail.drafts.insert(
        Draft(
            from: from,
            to: m.from,
            cc: theirs,
            subject: re(m.subject),
            body: signed(mail, from, quoted(m)),
            in_reply_to: m.message_id,
            references: "{m.references} {m.message_id}",
            conversation: conversation_of(m),
        ),
    )
}

/// Sends one message of the conversation on, with its sender, when and what it says, and the
/// files it came with.
action forward_message(m: Message) -> Draft (icon: .forward) {
    let mail = root Mail
    let from = sends_from(mail, m.account)
    mail.drafts.insert(
        Draft(
            from: from,
            subject: "Fwd: {m.subject}",
            body: signed(mail, from, forwarded(m)),
            attachments: m.attachments.filter(f -> on_device(f)),
        ),
    )
}

/// Sends the conversation's newest message on, with its sender, what it says and the files it
/// came with.
action forward(t: Thread) -> Draft (icon: .forward, key: "f") {
    let m = root Mail
    let last = t.messages.last()
    let from = sends_from(m, t.account)
    m.drafts.insert(
        Draft(
            from: from,
            subject: "Fwd: {t.subject}",
            body: signed(m, from, if let l = last { forwarded(l) } else { "" }),
            attachments: if let l = last { l.attachments.filter(f -> on_device(f)) } else { [] },
        ),
    )
}

/// `m` as a message sent on quotes it: who wrote it and when, then its lines after `> `, and the
/// files it came with that are not on this device, by name, since they cannot go with it.
fn forwarded(m: Message) -> Text {
    let left = m.attachments.filter(f -> not on_device(f)).map(f -> f.name)
    let names = left.join(", ")
    let note = if left.is_empty() {
        ""
    } else {
        std.say("Not downloaded, so not sent on: {names}")
    }
    let said = "\n\nFrom {m.from_name} <{m.from}> ({m.received}):\n\n> {m.body.replace("\n", "\n> ")}\n"
    if note == "" { said } else { "{said}\n{note}\n" }
}

/// Whether the bytes of `f` are on this device: a file a server's message named whose content
/// did not come with it has none.
fn on_device(f: File) -> Bool {
    f.url != "tilo-blob://"
}

/// One file the message came with, as a file of its own: what Save puts in Files, and what an
/// agent reads or hands another app.
action attachment(m: Message, name: Text) -> File (icon: .download, label: "Save attachment") {
    require m.attachments.any(f -> f.name == name) "It came with no {name}"
    require m.attachments.any(f -> f.name == name and on_device(f)) "{name} is not downloaded yet"
    m.attachments.find(f -> f.name == name and on_device(f)) ?? File.of_text(name, "")
}

/// Takes a file along with the message: one the person picks, or one an agent hands over. A
/// message takes up to sixteen files of up to 32 MB each, as mail servers take them.
action attach(d: Draft, file: File) (icon: .paperclip, label: "Attach a file") {
    require not d.sent or d.recalled "This message went already"
    require d.attachments.len() < 16 "A message takes up to 16 files"
    require file.size <= 33554432 "{file.name} is larger than 32 MB"
    d.attachments = [d.attachments, [file]].flat_map(x -> x)
}

/// Leaves a file the message was to take along behind.
action detach(d: Draft, name: Text) (icon: .x, label: "Remove attachment") {
    require not d.sent or d.recalled "This message went already"
    require d.attachments.any(f -> f.name == name) "It takes no {name}"
    d.attachments = d.attachments.filter(f -> f.name != name)
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

/// Deletes the conversation's trashed messages for good, from its server too, and the
/// conversation with them when no message of it is left. This is the one deletion there is no
/// trash to take back from: what is deleted for good is gone from the server, and this app's
/// record of it leaves once the server says it is.
destructive action purge(t: Thread) (icon: .trash, label: "Delete permanently") {
    require t.boxes.any(b -> b == "trash") "It is not in the trash"
    require t.messages.any(x -> x.mailbox == "trash") "Nothing of it is in the trash"
    let m = root Mail
    let doomed = t.messages.filter(x -> x.mailbox == "trash")
    // A conversation kept from outside an account's server has no server to be deleted from: it
    // goes from the workspace alone, at once. One whose server can be asked leaves the workspace
    // when the server says its messages are gone, so a deletion the server refused keeps it here.
    if has_account(m, t.account) {
        outside("imap.purge", (t.account, doomed.map(x -> x.key)))
    } else {
        for x in doomed {
            t.messages.remove(x)
        }
        let left = t.messages.len()
        if left == 0 {
            if m.recovered_mail.any(x -> x.id() == t.id()) { m.recovered_mail.remove(t) }
        } else {
            t.unread = t.messages.any(x -> x.unread)
            t.boxes.remove("trash")
        }
    }
}

/// Deletes every conversation in the account's trash from the mailbox and its server for good,
/// each as `purge` does. A view offers this behind a confirmation, as nothing undoes it.
destructive action empty_trash(a: Account) (icon: .trash, label: "Empty trash") {
    require not a.threads.matching("is:trash").is_empty() "The trash is empty"
    let trashed = a.threads.matching("is:trash")
    // The list is taken first: each purge may take its conversation out of the query as it
    // goes, and a query read while it changes answers nothing.
    for t in trashed {
        purge(t)
    }
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
/// workspace's labels the first time it is given, so it has a filter of its own from then on.
action label(t: Thread, name: Text, on: Bool = not t.labels.any(l -> l == name)) (icon: .tag) {
    require name != "" "A label needs a name"
    let m = root Mail
    if on { t.labels.add(name) } else { t.labels.remove(name) }
    if on and not m.labels.any(l -> l.name == name) { m.labels.insert(Label(name: name)) }
}

/// Renames one of the workspace's labels, all the conversations that carry it with it.
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
    for t in m.recovered_mail {
        if t.labels.any(x -> x == l.name) {
            t.labels.remove(l.name)
            t.labels.add(name)
        }
    }
    l.name = name
}

/// Deletes one of the workspace's labels, and takes it off every conversation that carries it.
destructive action delete_label(m: Mail, l: Label) (icon: .trash) {
    for a in m.accounts {
        for t in a.threads {
            if t.labels.any(x -> x == l.name) { t.labels.remove(l.name) }
        }
    }
    for t in m.recovered_mail {
        if t.labels.any(x -> x == l.name) { t.labels.remove(l.name) }
    }
    m.labels.remove(l)
}

/// Brings a message back to the outbox: it was sent to go at a moment still to come, and it is
/// a draft again, whatever the moment.
action cancel_later(d: Draft) (icon: .undo_2, label: "Cancel scheduled send") {
    require d.sent and not d.recalled "It is not waiting to go"
    require delivery(d)?.state != "sent" "It went already"
    withdraw(d, std.say("Scheduled send cancelled"))
    d.sent = false
    d.recalled = true
}

/// Sends the message at a moment instead of now: it waits in the outbox until then, as the
/// person's hold makes a message wait, and goes with no window open. Until it goes it can be
/// taken back, and bringing it back leaves it a draft.
action send_later(d: Draft, at: Time) uses time (icon: .clock, label: "Send later") {
    require not d.sent or d.recalled "This message went already"
    require d.to != "" or d.bcc != "" "It goes to nobody yet"
    require has_account(root Mail, d.from) "Add a mail account in Settings first"
    require at > now "Pick a moment still to come"
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
            d.invitation,
            d.attachments,
        ),
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

/// Opening a conversation reads it, wherever it is opened from: the list, search, a link or an
/// agent.
opened action read_on_open(t: Thread) {
    mark_read(t, false)
}

/// Sends the message from another workspace account.
action send_from(d: Draft, a: Account) (icon: .user) {
    require not d.sent or d.recalled "This message went already"
    d.from = a.key
    d.from_address = ""
}

/// Sends the message as another of its account's names or addresses, `Name <address>`: what a
/// person writing as a role rather than as themselves chooses. Empty sends as the account's own.
action send_as(d: Draft, identity: Text) (icon: .user, label: "Send as") {
    require not d.sent or d.recalled "This message went already"
    d.from_address = identity
}

/// The other names and addresses an account sends as, one each: none when it sends as one, and
/// none for no account.
fn identities_of(a: Option<Account>) -> List<Text> {
    if let acct = a {
        if acct.identities.trim() == "" { [] } else { acct.identities.split("\n") }
    } else { [] }
}

/// Throws a message being written away.
destructive action discard(m: Mail, d: Draft) (icon: .trash, label: "Delete draft") {
    m.drafts.remove(d)
}

/// Keeps the message in the account's server Drafts folder as it is written now, so another
/// mail client of the account sees it there too. What the server says comes back with the
/// account's next mail, and the server's Drafts are read like its other folders.
action save_to_drafts(m: Mail, d: Draft) (icon: .cloud, label: "Save in Drafts") {
    require not d.sent or d.recalled "This message is on its way"
    require has_account(m, d.from) "Add a mail account in Settings first"
    outside(
        "imap.save_draft",
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
            d.invitation,
        ),
    )
}

/// Sends a message the account's server refused again, from where it stands: the version that
/// failed did not leave, so it goes once. A message whose outcome nobody knows is not tried
/// again, since it may have gone.
action retry(d: Draft) uses time (icon: .refresh, label: "Try again") {
    require d.sent and not d.recalled "It is not waiting to go"
    require delivery(d)?.state == "rejected" "It did not fail, or nobody knows whether it went"
    d.sent = false
    send(d)
}

/// Tells everyone who reads the conversation that mail came to it: who wrote, what it is
/// called, and which account it came to while the workspace reads several of them, opening the
/// conversation. It is run once for each arrival, on the device that governs the workspace, so
/// each person is told once however many of their devices are open.
action announce(t: Thread) uses notify (icon: .bell, label: "Announce the mail that came") {
    require t.arrived != .none "No mail came to this conversation since it was last announced"
    // The moment is the announcement's: cleared here, whatever is done with it, so it tells
    // nobody twice.
    t.arrived = .none
    if t.muted or not t.boxes.any(b -> b == "inbox") {
        return
    }
    let m = root Mail
    if let newest = t.messages.last() {
        let title = if newest.from_name == "" { newest.from } else { newest.from_name }
        let account = m.accounts.find(a -> a.key == t.account)
        let body = if m.accounts.len() > 1 and account != .none {
            "{t.subject}\n{account?.address}"
        } else { t.subject }
        // The mailbox is the workspace's: everyone who is in it is told, so the mail reaches
        // each of their devices.
        for who in workspace.members {
            notify(to: who, title: title, body: body, opens: t)
        }
    }
}

/// Mail coming to a conversation's inbox is announced to the people who read it, once, at the
/// moment it came.
trigger new_mail(t: Thread) at t.arrived: announce(t)

/// Why a message cannot go from where it stands, as the window says it: where an account is
/// added when the workspace has none, and which account a message written before one picks.
fn send_refusal(m: Mail) -> Text {
    if m.accounts.is_empty() {
        "Add a mail account in Settings first"
    } else { "Pick the account it goes from" }
}

/// Sends the message from its account. It leaves the system, so it goes once, and waits in the
/// outbox until the server took it; it is held back for the person's hold first, so they can
/// take it back. The effect is an argument the action records, so the wait survives a window
/// closing and an agent takes it back the same way.
primary action send(d: Draft) uses time proposal compose(d) (icon: .send) {
    require not d.sent or d.recalled "This message went already"
    require d.to != "" or d.bcc != "" "It goes to nobody yet"
    require d.from != "" "Pick the account it goes from"
    require has_account(root Mail, d.from) "Add a mail account in Settings first"
    applies has_account(root Mail, d.from) send_refusal(root Mail)
    let m = root Mail
    let at = now + Duration(m.hold)
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
            d.invitation,
            d.attachments,
        ),
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
    withdraw(d, std.say("Taken back before it went"))
    d.sent = false
    d.recalled = true
}

/// Answers the conversation with what was typed: the message goes to whoever wrote its newest,
/// as a reply to it, quoted.
action answer(t: Thread, body: Text) -> Draft uses time (icon: .reply) {
    require body.len() > 0 "Say something first"
    require can_answer(t) "The account for this conversation is unavailable"
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
            conversation: t.key,
        ),
    )
    send(d)
    d
}

fn can_answer(t: Thread) -> Bool {
    let m = root Mail
    m.accounts.any(a -> a.key == t.account)
}

/// The message as it will leave, shown when someone is asked to send it.
view compose(d: Draft) {
    stack(gap: .related) {
        text("To {d.to}", role: .title)
        if d.bcc != "" {
            text("Bcc {d.bcc}", role: .caption)
        }
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
        std.say("All mail")
    } else {
        filters(Thread).find(f -> f.query == query)?.label
            ?? m.labels.find(l -> label_query(l.name) == query)?.name
            ?? std.say("All mail")
    }
}

/// What the account's server says of itself, over the mail it holds: the notice in the list's
/// own gutter, so its rounded card stands where the rows' boxes do. An account that stands well
/// says nothing and takes no room, so the list stands at the column's edge. A sign-in the server
/// refused, an account with no password on this device, and one whose password the workspace's
/// rules keep back each say what they are and lead to the Settings that change them; a device
/// that is offline says new mail waits for the connection.
view account_notice(a: Account) {
    if a.trouble != "" {
        stack(padding: .group, gap: .tight) {
            if a.trouble_kind == "signed-out" {
                ui.Banner(
                    "Can't sign in to {a.address}",
                    message: "Check its password in Settings, under Accounts.",
                    kind: .warning,
                    action: "Try again",
                ) {
                    on act: check_now(a)
                }
                ui.Link("settings://accounts", label: "Open account settings")
            } else if a.trouble_kind == "no-password" {
                ui.Banner(
                    "Mail has no password for {a.address}",
                    message: "Nothing was sent to its server. Enter its password in Settings, under Accounts.",
                    kind: .warning,
                    action: "Try again",
                ) {
                    on act: check_now(a)
                }
                ui.Link("settings://accounts", label: "Open account settings")
            } else if a.trouble_kind == "withheld" {
                ui.Banner(
                    "Mail can't use {a.address}'s password",
                    message: a.trouble,
                    kind: .warning,
                    action: "Try again",
                ) {
                    on act: check_now(a)
                }
                ui.Link("settings://accounts", label: "Open account settings")
            } else if a.trouble_kind == "offline" {
                ui.Banner(
                    "You're offline",
                    message: "New mail arrives when you're back online.",
                    kind: .neutral,
                    icon: .wifi_off,
                    action: "Try again",
                ) {
                    on act: check_now(a)
                }
            } else {
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
}

/// The mail window: every account's mail. The unified inbox is every account's inbox in one
/// list, a conversation in any account opening beside it; one account's window is its
/// mailboxes as its places.
view mail(m: Mail) {
    if m.accounts.is_empty() {
        without_accounts(m)
    } else {
        unified(m)
    }
}

/// The mail window with no account to read from: the drafts a person wrote, which need no
/// account, and the mail kept from the last account before it went. What sends a message is an
/// account, so the window says where one is added.
view without_accounts(m: Mail) {
    state place: Text = "drafts"
    let drafts = m.drafts.matching("is:drafts")
    let recovered = place == "recovered"
    ui.NavShell(
        content_kind: .document,
        title: if recovered { "Recovered mail" } else { "Drafts" },
        sidebar_title: "Mail",
        overview_title: if recovered { "Recovered mail" } else { "Drafts" },
        overview_count: if recovered { m.recovered_mail.len() } else { drafts.len() },
        actions: [new_draft],
    ) {
        sidebar: {
            ui.NavList(selected: place) {
                ui.NavItem("drafts", "Drafts", icon: .pencil, count: drafts.len()) {
                    on select: set(place, "drafts")
                }
                if not m.recovered_mail.is_empty() {
                    ui.NavItem("recovered", "Recovered mail", icon: .archive) {
                        on select: set(place, "recovered")
                    }
                }
            }
        }
        overview: {
            // A conversation recovered from the last account stands in the window's overview,
            // so one a person opens stands in the content beside it rather than over a page
            // with nothing left to show.
            if recovered {
                recovered_mail(m)
            } else {
                draft_list(drafts)
            }
        }
        content: {
            stack(gap: .group) {
                ui.EmptyState("Add a mail account in Settings", icon: .mail)
                ui.Link("settings://external_accounts", label: "Add an account in Settings")
            }
        }
    }
}

/// Every account's mail in one window: the unified inbox first, then each account's mail, and
/// each account's own places under it. A conversation shows which account it came to.
view unified(m: Mail) {
    state query: Text = "is:inbox"
    let places = filters(Thread).filter(f -> f.toggle)
    let drafts = m.drafts.matching("is:drafts")
    let title = if query == "is:drafts" { "Drafts" } else { place_title(m, query) }
    let shown: List<Thread> = if query == "is:drafts" or query == "recovered" {
        []
    } else { everywhere(m, query) }
    let waiting = waiting_to_go(m)
    ui.NavShell(
        content_kind: .document,
        title: if query == "recovered" {
            "Recovered mail"
        } else if query == "outbox" { "Outbox" } else { title },
        sidebar_title: "All accounts",
        overview_title: if query == "recovered" {
            "Recovered mail"
        } else if query == "is:drafts" {
            "Drafts"
        } else if query == "outbox" { "Outbox" } else { title },
        overview_count: if query == "is:drafts" {
            drafts.len()
        } else if query == "recovered" {
            m.recovered_mail.len()
        } else if query == "outbox" { waiting.len() } else { shown.len() },
        actions: [new_draft],
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
                ui.NavItem("is:drafts", "Drafts", icon: .pencil, count: drafts.len()) {
                    on select: set(query, "is:drafts")
                }
                if not m.recovered_mail.is_empty() {
                    ui.NavItem("recovered", "Recovered mail", icon: .archive) {
                        on select: set(query, "recovered")
                    }
                }
                if not waiting.is_empty() {
                    ui.NavItem("outbox", "Outbox", icon: .send, count: waiting.len()) {
                        on select: set(query, "outbox")
                    }
                }
                if not m.labels.is_empty() {
                    ui.NavSection("Labels") {
                        for l in m.labels {
                            ui.NavItem(
                                label_query(l.name),
                                l.name,
                                icon: .tag,
                                count: everywhere(m, label_query(l.name)).len(),
                            ) {
                                on select: set(query, label_query(l.name))
                            }
                        }
                    }
                }
                for acct in m.accounts {
                    ui.NavSection(acct.address) {
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
            // Every account's mail of the place is one list, so a conversation of any of them
            // stands in it, and what each account's server says of itself is said over it. The
            // drafts need no account, so they stand in the same window.
            if query == "is:drafts" {
                draft_list(drafts)
            } else if query == "recovered" {
                recovered_mail(m)
            } else if query == "outbox" {
                outbox(m)
            } else if query == "is:trash" {
                stack(fill, gap: .none) {
                    for acct in m.accounts {
                        trash_bar(acct)
                    }
                    accounts_threads(m, shown, query)
                }
            } else {
                accounts_threads(m, shown, query)
            }
        }
    }
}

view recovered_mail(m: Mail) {
    thread_list(m.recovered_mail.matching(""), more: .none)
}

/// What waits in the outbox: the messages that were sent and nothing carried them out yet, and
/// the ones the account's server refused, whichever account they go from.
fn waiting_to_go(m: Mail) -> List<Draft> {
    m.drafts.matching("is:outbox").filter(d -> delivery(d)?.state != "sent")
}

/// The outbox as a place of its own: every message that waits to go, how it stands, and the
/// presses that take one back or try a refused one again.
view outbox(m: Mail) {
    outbox_list(waiting_to_go(m))
}

view outbox_list(waiting: List<Draft>) {
    if waiting.is_empty() {
        ui.EmptyState("Nothing is waiting to be sent", icon: .send)
    } else {
        ui.VirtualList(selection_follows_focus: true, in_column: true) {
            on select(x: Draft): open(x, front: false)
            on activate(x: Draft): open(x)
            for d in waiting {
                ui.ListItem(item: d) {
                    draft_row(d)
                }
            }
        }
    }
}

/// Emptying the trash, behind a confirmation: nothing undoes it, so the first press says what
/// it would do and the second does it.
view trash_bar(a: Account) {
    state confirming: Bool
    let trashed = a.threads.matching("is:trash")
    if not trashed.is_empty() {
        stack(padding: .group) {
            ui.Banner(
                if confirming {
                    "Delete {trashed.len()} conversations permanently?"
                } else { "Empty the trash" },
                message: if confirming {
                    "This can't be undone."
                } else {
                    "Every conversation in it is deleted from this mailbox and the server."
                },
                kind: if confirming { .error } else { .neutral },
                icon: .trash,
                action: if confirming { "Delete permanently" } else { "Empty trash" },
            ) {
                on act: if confirming { empty_trash(a) } else { set(confirming, true) }
            }
        }
    }
}

/// The workspace's mail, with every account's inbox in one
/// list of conversations, newest first, with none of the window's sidebar or bar around it. The
/// mail window shows the same list under its places (`unified`).
view all_inboxes(m: Mail) {
    if m.accounts.is_empty() {
        ui.EmptyState("Add a mail account in Settings", icon: .mail)
    } else {
        accounts_threads(m, everywhere(m, "is:inbox"), "is:inbox")
    }
}

/// Every account's conversations of the place `query`, newest first: the union a window over all
/// the accounts reads, so a conversation of any of them stands in it and counts with the rest.
/// Each account's list is taken newest first, as its collection is read, and the union again
/// after the join.
fn everywhere(m: Mail, query: Text) -> List<Thread> {
    m.accounts
        .flat_map(a -> a.threads.matching(query).sort_by(x -> x.latest, desc))
        .sort_by(t -> t.latest, desc)
}

/// Every account's conversations of the place `query` as one list, newest first: what a window
/// over all the accounts lists, so a conversation of any of them stands in it and counts with
/// the rest. The list reads the page before it from the server of the first account whose
/// mailbox holds more as the person reaches its end, and under it each such account still says
/// so with a button.
view accounts_threads(m: Mail, shown: List<Thread>, query: Text) {
    // The account whose page the list reads as the person reaches its end: the first whose
    // server holds more, or, once all of them are read, the first whose mailboxes hold the
    // place, so the list stands as the person left it rather than being built again.
    let held = m.accounts.find(a -> a.mailboxes.any(b -> "is:{b.key}" == query))
    let next = m.accounts.find(a -> a.mailboxes.any(b -> "is:{b.key}" == query and b.more)) ?? held
    stack(fill, gap: .none) {
        for a in m.accounts {
            account_notice(a)
        }
        if let a = next, b = mailbox_place(a, query) {
            thread_list(shown, fill: true, more: .some(older_messages(b)))
        } else {
            thread_list(shown, fill: true, more: .none)
        }
        for a in m.accounts {
            if let b = mailbox_place(a, query) {
                if b.more {
                    row {
                        spacer()
                        ui.Button("Show older messages from {a.address}", icon: .arrow_down) {
                            on press: older_messages(b)
                        }
                    }
                }
            }
        }
    }
}

/// The mailbox a place shows of the account `a`, when the place is one of its mailboxes.
fn mailbox_place(a: Account, query: Text) -> Option<Mailbox> {
    a.mailboxes.find(b -> "is:{b.key}" == query)
}

/// Conversations of the account `a` as a list, under what its server says of itself, and
/// `Try again` asks the server that account reads from. The notice keeps its own gutter, so the
/// list stands under it with none between. When the place `query` is one of its mailboxes, the
/// list reads the page before it as the person reaches its end, and the button under it does
/// the same for a person who reaches its end with the keyboard.
view account_threads(a: Account, shown: List<Thread>, query: Text) {
    let place = mailbox_place(a, query)
    stack(fill, gap: .none) {
        account_notice(a)
        if let b = place {
            thread_list(shown, fill: true, more: .some(older_messages(b)))
        } else {
            thread_list(shown, fill: true, more: .none)
        }
        if let b = place {
            if b.more {
                row {
                    spacer()
                    ui.Button("Show older messages", icon: .arrow_down) {
                        on press: older_messages(b)
                    }
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
        title: a.address,
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
            if query == "is:trash" {
                stack(fill, gap: .none) {
                    trash_bar(a)
                    account_threads(a, shown, query)
                }
            } else {
                account_threads(a, shown, query)
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
        account_threads(a, in_mailbox(a, b), "is:{b.key}")
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
/// there are none. More than one row is chosen at a time — Ctrl+A chooses the whole list — and
/// the window's bar runs what the chosen ones' menus offer as one command over all of them, so
/// archiving, trashing, reading or deleting for good takes one press and one undo. A place whose
/// server holds more reads the page before the rows as the person reaches the list's end
/// (`more`); the button the place puts under the list asks the same, for a person who reaches
/// the end with the keyboard or a reader.
view thread_list(shown: List<Thread>, more: Option<Action>) {
    if shown.is_empty() {
        ui.EmptyState("Nothing here", icon: .inbox)
    } else if let next = more {
        ui.VirtualList(selection_follows_focus: true, multi_select: true, in_column: true) {
            on select(x: Thread): open(x, front: false)
            on activate(x: Thread): open(x)
            on end: next
            for x in shown {
                ui.ListItem(item: x, presence: lenses(x).find(l -> not l.mine)?.who) {
                    thread_row(x)
                }
            }
        }
    } else {
        ui.VirtualList(selection_follows_focus: true, multi_select: true, in_column: true) {
            on select(x: Thread): open(x, front: false)
            on activate(x: Thread): open(x)
            for x in shown {
                ui.ListItem(item: x, presence: lenses(x).find(l -> not l.mine)?.who) {
                    thread_item(x)
                }
            }
        }
    }
}

/// One conversation in a list: who wrote, the subject, the start of the newest message and
/// when it came; flagged, unread, or carrying files.
view thread_row(t: Thread) {
    row(
        t.senders,
        secondary: if t.snippet == "" { t.subject } else { "{t.subject} – {t.snippet}" },
        meta: t.latest,
        mark: if t.flagged {
            .flag
        } else if t.unread {
            .mail
        } else if t.messages.any(m -> not m.attachments.is_empty()) { .paperclip },
    )
}

/// A conversation's item in the list: its row, and the preview of its newest message, which
/// shows what it says without opening the conversation, so reading it marks nothing read.
view thread_item(t: Thread) {
    thread_row(t)
    if let m = t.messages.last() {
        ui.HoverToolbar {
            ui.Button("Preview", icon: .eye, icon_only: true) {
                on press: open(m, view: inspect)
            }
        }
        ui.ContextMenu {
            ui.MenuItem("Preview", icon: .eye) {
                on choose: open(m, view: inspect)
            }
        }
    }
}

/// The messages being written, as the Drafts place lists them: each a row that opens the draft
/// beside it. The list asks for the rows in view, so a workspace with years of drafts opens as
/// fast as one with a few.
view draft_list(shown: List<Draft>) {
    if shown.is_empty() {
        ui.EmptyState("No drafts", icon: .pencil)
    } else {
        ui.VirtualList(selection_follows_focus: true, in_column: true) {
            on select(x: Draft): open(x, front: false)
            on activate(x: Draft): open(x)
            for x in shown {
                ui.ListItem(item: x) {
                    draft_row(x)
                }
            }
        }
    }
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
        std.say("Taken back, and a draft again")
    } else if let s = delivery(d) {
        if s.state == "sent" {
            std.say("Sent")
        } else if s.state == "sending" {
            std.say("Sending")
        } else if s.state == "rejected" {
            std.say("Not sent: {s.reason}")
        } else if s.state == "unknown" {
            std.say("Not known whether it was sent")
        } else if s.state == "held" {
            std.say("Sending in a moment")
        } else if s.reason == "" {
            std.say("Waiting to send")
        } else {
            std.say("Waiting to go online: {s.reason}")
        }
    } else { "" }
}

/// Whether a message was sent and can still be taken back: nothing carried it out yet.
fn can_undo(d: Draft) -> Bool {
    d.sent and not d.recalled and delivery(d)?.state != "sent"
}

/// A conversation's window: its subject, then every message in order with who wrote it, to
/// whom and when, what it says and what came with it, and a reply at the foot. A conversation
/// in the trash offers to delete it permanently, behind a confirmation, since nothing undoes it.
view conversation(t: Thread) {
    state draft: Text
    state confirming: Bool
    // A conversation no account of this workspace sent has nothing to reply from, so its window
    // keeps no reply bar rather than an empty one.
    let answered = can_answer(t)
    ui.NavShell(content_kind: .page, title: t.subject) {
        content: {
            if answered {
                ui.Docked {
                    // The offer to delete it for good stays above the reply bar, at the window's
                    // foot, so the messages keep the room they need.
                    foot: {
                        stack(gap: .none) {
                            if t.boxes.any(b -> b == "trash") {
                                trash_offer(t, confirming)
                            }
                            reply_bar(t, draft)
                        }
                    }
                    conversation_messages(t)
                }
            } else {
                stack(fill, gap: .none) {
                    if t.boxes.any(b -> b == "trash") {
                        trash_offer(t, confirming)
                    }
                    conversation_messages(t)
                }
            }
        }
    }
}

/// The offer to delete a trashed conversation for good, behind a confirmation: nothing undoes
/// it, so the first press says what it would do and the second does it.
template view trash_offer(t: Thread, confirming: State<Bool>) {
    stack(padding: .group) {
        ui.Banner(
            if confirming {
                "Delete this conversation permanently?"
            } else { "It is in the trash" },
            message: if confirming {
                "This can't be undone."
            } else {
                "It stays here until you delete it permanently."
            },
            kind: if confirming { .error } else { .neutral },
            icon: .trash,
            action: "Delete permanently",
        ) {
            on act: if confirming { purge(t) } else { set(confirming, true) }
        }
    }
}

/// The bar a conversation is answered from: what is typed, and the press that sends it.
template view reply_bar(t: Thread, draft: State<Text>) {
    row {
        ui.TextInput(bind: draft, placeholder: "Reply to {t.subject}", fill) {
            on submit: answer(t, draft).then(set(draft, ""))
        }
        ui.Button("Reply", icon: .send, icon_only: true) {
            on press: answer(t, draft).then(set(draft, ""))
        }
    }
}

/// A conversation's messages, in order: the list is the window body's scroller.
view conversation_messages(t: Thread) {
    ui.VirtualList(page_column: true, at_measure: true) {
        for m in t.messages {
            ui.ListItem(item: m) {
                message(m)
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
            ui.Avatar(name: if m.from_name == "" { m.from } else { m.from_name })
            ui.Text(if m.from_name == "" { m.from } else { m.from_name }, role: .subheading)
            ui.Time(m.received, role: .caption, past: true, secondary: true)
            spacer()
            ui.HoverToolbar {
                // Who wrote it, when the workspace's address book knows them: the person the
                // message is from opens in Contacts, where their numbers and groups are.
                if let who = contacts.for_address(contacts.book, sender_address(m.from)) {
                    ui.Button("Open contact", icon: .user, icon_only: true) {
                        on press: open(who)
                    }
                }
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
            if let who = contacts.for_address(contacts.book, sender_address(m.from)) {
                ui.MenuItem("Open contact", icon: .user) {
                    on choose: open(who)
                }
            }
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
            if not m.attachments.is_empty() {
                row(gap: .tight, wrap: true) {
                    for f in m.attachments {
                        attachment_chip(m, f)
                    }
                }
            }
        }
    }
}

/// A file a message came with: its name, which saves it to Files when pressed, or says it is not
/// downloaded yet.
view attachment_chip(m: Message, f: File) {
    if on_device(f) {
        ui.Chip(f.name, icon: .paperclip) {
            on press: file.save(attachment(m, f.name), name: f.name)
        }
    } else {
        ui.Chip(std.say("{f.name}, not downloaded"), icon: .paperclip)
    }
}

/// One message as it came, shown without its conversation opening: what a person reads before
/// deciding to answer. Nothing is marked read here, and the sender is told nothing; opening the
/// conversation is what reads it.
view inspect(m: Message) {
    let subject = if m.subject.trim() == "" { "No subject" } else { m.subject }
    ui.NavShell(content_kind: .page, title: subject) {
        content: {
            ui.Scroll {
                page {
                    stack(gap: .section) {
                        if m.unread {
                            ui.Banner(
                                "Unread",
                                message: "It stays unread until its conversation is opened.",
                                kind: .info,
                            )
                        }
                        row(gap: .related, align: .center) {
                            ui.Text(
                                if m.from_name == "" { m.from } else { m.from_name },
                                role: .subheading,
                            )
                            ui.Time(m.received, role: .caption, past: true, secondary: true)
                        }
                        ui.Text("to {m.to}", role: .caption, secondary: true)
                        ui.Card {
                            ui.Text(as_lines(m.body), role: .body)
                            if not m.attachments.is_empty() {
                                row(gap: .tight) {
                                    for f in m.attachments {
                                        attachment_chip(m, f)
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A message being written: its window.
view editor(d: Draft) {
    // The formatting controls ask the shared editor for a command, told apart by a count so the
    // same command pressed twice is two commands.
    state formats: Int = 0
    state format_ask: Option<Text> = .none
    state link_open: Bool = false
    state link_target: Text = ""
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
        let mail = root Mail
        let sender = mail.accounts.find(a -> a.key == d.from)
        field("From") {
            // One account a draft goes from is said; with more, or none of them chosen yet, each
            // is offered. With no account at all, nothing can send it: the person is told so and
            // offered the way to where an account is added. An account that sends as more than
            // one name or address offers each under it: the one it hears back from is the one the
            // person chose.
            if mail.accounts.is_empty() {
                stack(gap: .tight) {
                    text("No mail account to send from")
                    ui.Link("settings://external_accounts", label: "Add an account in Settings")
                }
            } else if mail.accounts.len() == 1 and sender != .none {
                if identities_of(sender).is_empty() {
                    text(sender?.address ?? "")
                } else {
                    row(gap: .tight, wrap: true) {
                        ui.Chip(sender?.address ?? "", selected: d.from_address == "") {
                            on press: send_as(d, "")
                        }
                        for identity in identities_of(sender) {
                            ui.Chip(identity, selected: d.from_address == identity) {
                                on press: send_as(d, identity)
                            }
                        }
                    }
                }
            } else {
                row(gap: .tight, wrap: true) {
                    for a in mail.accounts {
                        ui.Chip(a.address, selected: a.key == d.from) {
                            on press: send_from(d, a)
                        }
                    }
                }
                if not identities_of(sender).is_empty() {
                    row(gap: .tight, wrap: true) {
                        for identity in identities_of(sender) {
                            ui.Chip(identity, selected: d.from_address == identity) {
                                on press: send_as(d, identity)
                            }
                        }
                    }
                }
            }
        }
        field("To") {
            input(bind: d.to, placeholder: "name@example.org")
        }
        field("Cc") {
            input(bind: d.cc)
        }
        field("Bcc") {
            input(bind: d.bcc, placeholder: "name@example.org")
        }
        input(bind: d.subject, placeholder: "Subject")
        // The formatting the shared editor applies, asked of it by these controls: what a person
        // presses reaches the words as formatting, which the message carries to its recipients
        // as its formatted alternative beside the words themselves.
        row(gap: .tight, align: .center) {
            ui.Button("Bold", icon: .bold, icon_only: true) {
                on press: set(formats, formats + 1).then(set(format_ask, "bold:{formats}"))
            }
            ui.Button("Italic", icon: .italic, icon_only: true) {
                on press: set(formats, formats + 1).then(set(format_ask, "italic:{formats}"))
            }
            ui.Button("Underline", icon: .underline, icon_only: true) {
                on press: set(formats, formats + 1).then(set(format_ask, "underline:{formats}"))
            }
            ui.Button("Bulleted list", icon: .list, icon_only: true) {
                on press: set(formats, formats + 1).then(set(format_ask, "bullets:{formats}"))
            }
            ui.Button("Link", icon: .link, icon_only: true) {
                on press: set(link_open, not link_open)
            }
            spacer()
        }
        if link_open {
            row(gap: .tight) {
                ui.TextInput(bind: link_target, placeholder: "https://example.org", fill) {
                    on submit: set(formats, formats + 1)
                        .then(
                            set(format_ask, "link:{link_target}:{formats}"),
                        )
                        .then(set(link_open, false))
                        .then(set(link_target, ""))
                }
                ui.Button("Add link", icon: .check, icon_only: true) {
                    on press: set(formats, formats + 1)
                        .then(
                            set(format_ask, "link:{link_target}:{formats}"),
                        )
                        .then(set(link_open, false))
                        .then(set(link_target, ""))
                }
            }
        }
        ui.BlockEditor(bind: d.body, placeholder: "Write your message", format: format_ask)
        if not d.attachments.is_empty() {
            row(gap: .tight, wrap: true) {
                for f in d.attachments {
                    ui.Chip(f.name, icon: .paperclip) {
                        on remove: detach(d, f.name)
                    }
                }
            }
        }
        // The message's own Send: a composer sends from where it is written, and the window's
        // bar keeps it once for the draft it shows. With no account to go from, the way to add
        // one stands where Send does: pressing it opens the Settings page that adds an account,
        // and the draft stays as it is. With one, the draft can also be kept in the account's
        // server Drafts folder for its other mail clients.
        row {
            ui.Button("Attach", icon: .paperclip) {
                on press: file.choose(attach(d))
            }
            spacer()
            if mail.accounts.is_empty() {
                ui.Link(
                    "settings://external_accounts",
                    label: "Add an account to send",
                    role: .label,
                )
            } else {
                ui.Button("Save in Drafts", icon: .cloud) {
                    on press: save_to_drafts(mail, d)
                }
                ui.Button("Send", icon: .send) {
                    on press: send(d)
                }
            }
        }
        // An answer shows the conversation it answers under it, so the person reads what their
        // message goes into without leaving the draft.
        if let t = thread_named(mail, d.conversation) {
            std.after_body {
                stack(gap: .group) {
                    ui.Text("This answers {t.subject}", role: .heading)
                    for m in t.messages {
                        stack(gap: .tight) {
                            row(gap: .related, align: .center) {
                                ui.Text(
                                    if m.from_name == "" { m.from } else { m.from_name },
                                    role: .subheading,
                                )
                                ui.Time(m.received, role: .caption, past: true, secondary: true)
                            }
                            ui.Text(as_lines(m.body), role: .body, secondary: true)
                        }
                    }
                }
            }
        }
    }
}

/// An inbox a working week in: conversations to read, some answered, one flagged, a few
/// archived, sent and junk, the launch conversation open.
///
/// The account, its mailboxes and its mail are what the account's server holds, which no action
/// of this app writes: a seeded workspace or a staged window takes them in from the sample mail
/// server through the mail service, as a real server's answer comes in, before this runs. What
/// is left is the person's: the conversation the window opens.
scenario "a busy inbox" -> Thread {
    let m = root Mail
    if let launch = thread_named(m, "launch@rivera.example") {
        launch
    } else {
        Thread(
            key: "launch@rivera.example",
            account: "",
            subject: "Launch plan for the autumn release",
            latest: 2026-06-30T08:20,
            snippet: "",
            senders: "Grace Hopper",
            count: 1,
            unread: true,
        )
    }
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
    // The conversation the reply answers stands in the inbox beside it.
    let launch = a.threads.insert(
        Thread(
            key: "launch@rivera.example",
            account: a.key,
            subject: "Launch plan for the autumn release",
            latest: 2026-06-30T08:20,
            snippet: "Here is the draft plan.",
            senders: "Grace Hopper",
            count: 1,
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
    m.drafts.insert(
        Draft(
            from: a.key,
            to: "grace@hopper.example, ada@analytical.example",
            cc: "support@rivera.example",
            subject: "Re: Launch plan for the autumn release",
            body: "The fifth works for the beta. I would move the release to the fourteenth so the docs can catch up.\n\nSam",
            in_reply_to: "launch@rivera.example",
            references: "launch@rivera.example",
            conversation: launch.key,
        ),
    )
}

/// A reply to mail kept from an account that went, with no account left to send it from.
scenario "a reply with no account to send it" -> Draft {
    let m = root Mail
    m.drafts.insert(
        Draft(
            to: "desk@parcels.example",
            subject: "Re: Your parcel was delivered",
            body: "Thank you, I collected it from number twelve.",
            in_reply_to: "delivery@parcels.example",
            references: "delivery@parcels.example",
        ),
    )
}

/// No account yet: what a first start shows.
scenario "no account yet" {
    root Mail
}

/// A conversation in the trash, and its window: the offer to delete it for good stands over it.
scenario "a conversation in the trash" -> Account {
    let m = root Mail
    let a = m.accounts.insert(
        Account(
            key: "sam@rivera.example",
            address: "sam@rivera.example",
            name: "Sam Rivera",
            online: true,
        ),
    )
    a.mailboxes.insert(Mailbox(key: "inbox", name: "Inbox", role: "inbox"))
    a.mailboxes.insert(Mailbox(key: "trash", name: "Trash", role: "trash", total: 1))
    let t = a.threads.insert(
        Thread(
            key: "offer@deals.example",
            account: a.key,
            subject: "You have won a cruise",
            latest: 2026-06-26T03:12,
            snippet: "Claim your prize within 24 hours.",
            senders: "Prize Draw",
            count: 1,
        ),
    )
    t.boxes.add("trash")
    t.messages.insert(
        Message(
            key: "offer@deals.example",
            account: a.key,
            subject: "You have won a cruise",
            from: "win@deals.example",
            from_name: "Prize Draw",
            to: "sam@rivera.example",
            received: 2026-06-26T03:12,
            snippet: "Claim your prize.",
            body: "Claim your prize within 24 hours of confirming your card details.",
            message_id: "offer@deals.example",
            mailbox: "trash",
        ),
    )
    a
}

/// Messages waiting to go, and the window's Outbox place over them.
scenario "messages waiting to go" {
    let m = root Mail
    let a = m.accounts.insert(
        Account(
            key: "sam@rivera.example",
            address: "sam@rivera.example",
            name: "Sam Rivera",
            online: true,
        ),
    )
    a.mailboxes.insert(Mailbox(key: "inbox", name: "Inbox", role: "inbox"))
    a.mailboxes.insert(Mailbox(key: "sent", name: "Sent", role: "sent"))
    m.drafts.insert(
        Draft(
            from: a.key,
            to: "grace@hopper.example",
            subject: "Launch plan",
            body: "Here is where it stands.",
            sent: true,
            send_at: 2026-06-30T18:00,
        ),
    )
    m.drafts.insert(
        Draft(
            from: a.key,
            to: "ada@analytical.example",
            subject: "Engine notes",
            body: "Look at the timing.",
            sent: true,
            send_at: 2026-06-30T18:00,
        ),
    )
    root Mail
}

/// Mail the workspace read from the last account before it went: no account stands now, and
/// what was recovered is the window's own list.
scenario "mail recovered from the last account" {
    let m = root Mail
    let t = m.recovered_mail.insert(
        Thread(
            key: "delivery@parcels.example",
            account: "sam@rivera.example",
            subject: "Your parcel was delivered",
            latest: 2026-06-29T16:10,
            snippet: "It was left with the neighbour at number twelve.",
            senders: "Parcels",
            count: 1,
            unread: false,
        ),
    )
    t.messages.insert(
        Message(
            key: "delivery@parcels.example",
            account: "sam@rivera.example",
            subject: "Your parcel was delivered",
            from: "desk@parcels.example",
            from_name: "Parcels",
            to: "sam@rivera.example",
            received: 2026-06-29T16:10,
            snippet: "It was left with the neighbour at number twelve.",
            body: "Your parcel was delivered this afternoon and left with the neighbour at number twelve.\n\nParcels",
            message_id: "delivery@parcels.example",
            mailbox: "inbox",
        ),
    )
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
            mailbox: "archive",
        ),
    )
    inbox
}

/// A letter that came with a file: the conversation shows what it came with, each file a chip
/// that saves it to Files.
scenario "a letter with its papers" {
    let m = root Mail
    let a = m.accounts.insert(
        Account(
            key: "sam@rivera.example",
            address: "sam@rivera.example",
            name: "Sam Rivera",
            online: true,
        ),
    )
    a.mailboxes.insert(Mailbox(key: "inbox", name: "Inbox", role: "inbox", unread: 1, total: 1))
    let lease = a.threads.insert(
        Thread(
            key: "lease@lettings.example",
            account: a.key,
            subject: "Your lease renewal",
            latest: 2026-06-30T10:25,
            snippet: "The new lease is attached. Please sign by Friday.",
            senders: "Mill Lane Lettings",
            count: 1,
            unread: true,
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
            mailbox: "inbox",
            attachments: [
                File.of_text(
                    "Lease.md",
                    "# Tenancy agreement\n\nThe rent is due on the first of every month, and the term runs for a year.",
                ),
            ],
        ),
    )
}

/// A conversation the person deleted: it stands in the trash, where the place offers to empty
/// it and the conversation can be deleted for good.
scenario "a conversation in the trash" {
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
    a.mailboxes.insert(Mailbox(key: "trash", name: "Trash", role: "trash", total: 1))
    let dinner = a.threads.insert(
        Thread(
            key: "dinner@bo.example",
            account: a.key,
            subject: "Dinner on Friday?",
            latest: 2026-06-30T08:51,
            snippet: "Shall we try the new place on Mill Lane? I can book for eight.",
            senders: "Bo Chen",
            count: 1,
        ),
    )
    dinner.boxes.add("trash")
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
            mailbox: "trash",
        ),
    )
}
