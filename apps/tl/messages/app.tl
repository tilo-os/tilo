app so.tilo.messages "Messages" (
    icon: .message_circle,
    category: .network,
    tagline: "Direct conversations across your messaging accounts",
    keywords: ["messaging", "direct messages", "dm", "imessage", "sms", "text messages", "whatsapp", "signal", "kurznachrichten", "textnachrichten", "mitteilungen", "nachrichten"],
)

use tilo.workspace as workspace
use tilo.accounts as accounts
use so.tilo.contacts.directory as contacts
use so.tilo.chat as chat
use so.tilo.session as session

// Messages reaches people on the services the person's accounts are on: Signal, WhatsApp and
// SMS through the device's messaging service, and Tilo through the workspace's own Chat. A
// service's history is the account's, never the workspace's: the device answers it as a job's
// record, which this app reads and never copies into a resource. What the workspace keeps is
// each person's unsent drafts, their window here, and the conversations agents and automations
// hand it to queue.
/// What the app keeps for the workspace: the conversations handed to it to queue, each person's
/// window and their unsent words.
type Messages {
    conversations: Collection<Conversation>
    sessions: Collection<Session>
    drafts: Collection<Draft>
}

root messages: Messages

default view messages_window for Messages

/// A conversation an agent, an automation or another app handed Messages: what was delivered
/// into it and what was queued in it, kept in this copy until a device that holds the account
/// sends it.
type Conversation (title: with, untitled: "Conversation") {
    /// Who it is with, as the one who started it named them: a name or a phone number.
    with: Text
    /// The key agents name it by: the phone number or name it is with, as `send_message` and
    /// `deliver_message` answer it.
    key: Text = ""
    messages: Collection<Message>

    search {
        kind conversation
    }
}

/// One message of a kept conversation.
type Message (title: body) {
    /// What the same message delivered twice is known by, so it is kept once.
    mark: Text = ""
    /// Who sent it, for a message that came in; empty for one written here.
    from: Text
    /// Who it is for, when that was said.
    to: Text
    body: Text
    /// When it was sent.
    at: Time
    /// Written here rather than received.
    outgoing: Bool = false
    /// Waiting for a device that holds the account to send it.
    queued: Bool = false

    search {
        kind message
    }
}

/// One person's window on Messages: who they are writing to, on which account, and the device's
/// work their window waits on. It is theirs alone (`readers: author`), and holds no message of a
/// service: what a history says lives in the job's record on the device that read it.
type Session (readers: author) {
    /// The workspace member the conversation is with.
    member: Option<Actor> = .none
    /// The address book's person the conversation is with, by the directory's id.
    contact: Text = ""
    /// The kept conversation open, by its key.
    held: Text = ""
    /// The way the person writes to them: a `Choice` as JSON, empty until one is chosen.
    choice: Text = ""
    /// What stands in the content column instead of a conversation: `setup` or `groups`.
    pane: Text = ""
    /// What the device last said of the person's accounts (`state`), as JSON.
    status: Text = ""
    /// The pages of the open conversation, by their jobs: the newest first.
    pages: List<Text>
    /// The send on its way, by its job, and the operation it is: a second try of the same
    /// operation is refused by the service, so a send never goes twice.
    send_job: Text = ""
    operation: Text = ""
    /// Why the last send did not go, by the service's word; empty after one that went.
    refused: Text = ""
    /// The account being linked to this device, and the job that links it.
    setup_service: Text = ""
    setup_account: Text = ""
    link_job: Text = ""
    linked_job: Text = ""
    /// The first account being set up: its service, the job that asks the device for the step,
    /// and what the device asked to be reviewed.
    first_service: Text = ""
    first_job: Text = ""
    first_review: Text = ""
    first_reviewed: Bool = false
    sims_job: Text = ""
    /// The groups listed, by account, and the group open.
    groups_jobs: List<Text>
    group_service: Text = ""
    group_account: Text = ""
    group: Text = ""
    group_job: Text = ""
    group_history_job: Text = ""
    group_change_job: Text = ""
}

/// What a person is writing to someone and has not sent, kept per way of writing to them: each
/// service and account keeps its own words. Read by its author alone.
type Draft (readers: author) {
    /// The way it is written: the choice's key.
    route: Text
    /// Who it is written to, as the window names them.
    person: Text
    body: RichText
}

/// One way of writing to a person: the service, the account it goes from, and the address it
/// reaches them at, with the directory's confirmation of that address. An account with no
/// address yet, or no account at all, is a choice too: the window says what it needs.
type Choice {
    service: Text = ""
    account: Text = ""
    recipient: Text = ""
    association: Text = ""
    scope: Text = ""
    revision: Text = ""
}

/// What the messaging service is handed for a conversation: the person, the confirmed address
/// and the account it is written from. The service checks every one of these again.
type Route {
    person: Text
    association: Text
    sender: Text
    service: Text
    scope: Text
    recipient: Text
    revision: Text
}

/// An account of the person's on a messaging service, as the service names it.
type ServiceAccount {
    service: Text
    account: Text
}

/// What the device is asked of the person's accounts.
type StateAsk {
    accounts: List<ServiceAccount>
    routes: List<Route>
}

/// What the device says of the person's accounts: whether the messaging service runs here,
/// whether it sets up first accounts and SIMs, and each account's state.
type Status {
    available: Bool = false
    first: Bool = false
    sims: Bool = false
    actor: Text = ""
    accounts: List<AccountStatus>
    routes: List<Bool>
}

type AccountStatus {
    service: Text = ""
    account: Text = ""
    state: Text = "unknown"
    phase: Text = "empty"
    linked: Bool = false
    linkable: Bool = false
    wake_ms: Int = 0
    groups: Bool = false
}

/// One page of a history: its messages, oldest first, and whether older ones remain.
type Page {
    messages: List<Line>
    more: Bool = false
}

/// One message of a service's history.
type Line {
    /// The service's id of the message.
    key: Text = ""
    text: Text = ""
    incoming: Bool = false
    at_ms: Int = 0
    at: Time
    delivery: Text = ""
    author: Text = ""
}

type HistoryAsk {
    route: Route
    before: Text
}

type SendAsk {
    route: Route
    operation: Text
    text: Text
}

/// What a send's job says once the service took it.
type Sent {
    operation: Text = ""
    status: Text = ""
}

/// Why a job ended without its answer, in the service's words.
type Why {
    why: Text = ""
}

/// The private code a phone scans to link an account to this device.
type Linking {
    service: Text = ""
    account: Text = ""
    attempt: Text = ""
    generation: Int = 0
    code: Text = ""
}

type FinishAsk {
    service: Text
    account: Text
    attempt: Text
}

/// What a first account's setup asks the person to review: the account, its scope, and the
/// number its service reports once it is linked.
type Review {
    service: Text = ""
    account: Text = ""
    scope: Text = ""
    number: Text = ""
}

type ReviewAsk {
    service: Text
    account: Text
    display_name: Text
}

type ServiceAsk {
    service: Text
}

type SimAsk {
    selection: Text
}

type Sims {
    choices: List<Sim>
}

type Sim {
    selection: Text = ""
    label: Text = ""
}

/// The groups of one account.
type Groups {
    service: Text = ""
    account: Text = ""
    groups: List<Group>
}

type Group {
    /// The service's id of the group.
    key: Text = ""
    title: Text = ""
}

type GroupAsk {
    service: Text
    account: Text
    group: Text
}

/// Who is in a group, as the account last read it, and the review a change or a send names.
type Membership {
    service: Text = ""
    account: Text = ""
    group: Text = ""
    title: Text = ""
    announcement: Bool = false
    self_member: Text = ""
    review: Text = ""
    members: List<Member>
}

type Member {
    address: Text = ""
    role: Text = "member"
}

type GroupHistory {
    group: Text = ""
    messages: List<Line>
}

type GroupSendAsk {
    service: Text
    account: Text
    group: Text
    review: Text
    operation: Text
    text: Text
}

type GroupChangeAsk {
    service: Text
    account: Text
    group: Text
    review: Text
    operation: Text
    change: Text
    member: Text
    route: Option<Route>
}

type GroupChanged {
    membership: Option<Membership> = .none
}

type GroupSent {
    history: Option<GroupHistory> = .none
    membership: Option<Membership> = .none
}

/// A message an export writes, with where it is.
type Found {
    conversation: Text
    with: Text
    from: Text
    to: Text
    body: Text
    date: Text
    at: Time
    outgoing: Bool
    queued: Bool
}

// ---------------------------------------------------------------------------------------------
// The services
/// The services Messages writes on through the device, in the order a person's choices list them.
fn services() -> List<Text> {
    ["whatsapp", "signal", "sms"]
}

/// What a service is called.
fn service_name(service: Text) -> Text {
    if service == "whatsapp" {
        "WhatsApp"
    } else if service == "signal" {
        "Signal"
    } else if service == "sms" { "SMS" } else if service == "tilo" { "Tilo" } else { service }
}

/// The person's enabled accounts on the messaging services, in the record's order.
fn service_accounts() -> List<accounts.AccountView> {
    accounts.accounts.accounts.filter(a -> a.enabled and services().contains(a.service))
}

/// An account's name, never its id: which account a thread arrives on is what a person needs.
fn account_name(id: Text) -> Text {
    if id == "" {
        "Not configured"
    } else if let a = accounts.accounts.accounts.find(a -> a.id == id) {
        if a.name != "" { a.name } else { a.address }
    } else { id }
}

/// What a choice reads as where the person picks it.
fn choice_label(c: Choice) -> Text {
    if c.service == "tilo" {
        "Tilo"
    } else if c.recipient == "" {
        "{service_name(c.service)} from {account_name(c.account)}"
    } else {
        "{service_name(c.service)} from {account_name(c.account)} to {c.recipient}"
    }
}

/// A choice as the window keeps it: its fields in one text, the same for the same choice.
fn choice_key(c: Choice) -> Text {
    json.encode(c)
}

fn choice_of(key: Text) -> Option<Choice> {
    if key == "" {
        return .none
    }
    match json.decode<Choice>(key) {
        .ok(c) => .some(c),
        .err(why) => .none,
    }
}

/// The route the service is handed for a choice made for `person`.
fn route_of(person: Text, c: Choice) -> Route {
    Route(
        person: person,
        association: c.association,
        sender: c.account,
        service: c.service,
        scope: c.scope,
        recipient: c.recipient,
        revision: c.revision,
    )
}

/// The directory's person an address of `service` belongs to, when exactly one confirmed
/// address answers it: an address two people claim, or one confirmed under another identity,
/// belongs to none, so it is never guessed.
fn resolve_account(
    d: contacts.Identities,
    service: Text,
    scope: Text,
    recipient: Text,
    revision: Text,
) -> Option<Text> {
    if scope == "" or revision == "" {
        return .none
    }
    let claims = d.people.filter(
        p -> not p.removed
            and p.endpoints.any(
                e -> not e.removed
                    and e.service == service
                    and e.scope == scope
                    and e.recipient == recipient,
            ),
    )
    if claims.len() != 1 {
        return .none
    }
    if let owner = claims.first() {
        let held = owner.endpoints.filter(
            e -> not e.removed
                and e.service == service
                and e.scope == scope
                and e.recipient == recipient,
        )
        if held.all(e -> e.revision == revision) {
            return .some(owner.id())
        }
    }
    .none
}

/// The ways of writing to the address book's `person`: for each service, every enabled account
/// whose scope one of the person's confirmed addresses there is confirmed under, ordered by
/// account and address; where none is, one choice per account (or one with none) that says what
/// is missing.
fn choices_of(d: contacts.Identities, person: Text) -> List<Choice> {
    let found = d.people.find(p -> p.id() == person and not p.removed)
    let out: List<Choice> = []
    if let p = found {
        for service in services() {
            let senders = service_accounts().filter(a -> a.service == service and a.id != "")
            let routes: List<Choice> = []
            for e in p.endpoints {
                if not e.removed and e.service == service {
                    if resolve_account(d, service, e.scope, e.recipient, e.revision)
                        == .some(person) {
                        for s in senders {
                            if s.scope == e.scope {
                                let c = Choice(
                                    service: service,
                                    account: s.id,
                                    recipient: e.recipient,
                                    association: e.id(),
                                    scope: e.scope,
                                    revision: e.revision,
                                )
                                if not routes.any(r -> choice_key(r) == choice_key(c)) {
                                    routes = routes + [c]
                                }
                            }
                        }
                    }
                }
            }
            if routes.is_empty() {
                if senders.is_empty() {
                    out = out + [Choice(service: service)]
                } else {
                    for s in senders {
                        out = out + [Choice(service: service, account: s.id)]
                    }
                }
            } else {
                out = out + routes.sort_by(r -> "{r.account}\t{r.association}")
            }
        }
    }
    out
}

/// The way of writing to a workspace member: Tilo, from the person, to them.
fn member_choice(who: Actor, me: Actor) -> Choice {
    Choice(service: "tilo", account: me.id(), recipient: who.id())
}

/// The members of the workspace a person writes to here: everyone but themselves.
fn members_of(me: Actor) -> List<Actor> {
    workspace.members.people.filter(p -> p != me)
}

/// The address book's people, without the workspace's members and those removed.
fn contacts_of(d: contacts.Identities) -> List<contacts.Person> {
    d.people.filter(p -> not p.removed).sort_by(p -> p.label.lower())
}

/// Opens Contacts, where the people Messages writes to are added.
action open_contacts(m: Messages) (icon: .users, label: "Open Contacts") {
    session.open_app("so.tilo.contacts")
}

/// The person's window, when they have opened one.
fn session_of(m: Messages) -> Option<Session> {
    m.sessions.first()
}

/// Whether the window shows `pane`: "setup" or "groups".
fn pane_is(m: Messages, pane: Text) -> Bool {
    if let s = session_of(m) { s.pane == pane } else { false }
}

/// Whether the setup shows and this device links a first account: messaging runs here.
fn first_here(m: Messages) -> Bool {
    pane_is(m, "setup") and status_of(session_of(m)).first
}

/// Whether the setup shows and this device has SIMs to send SMS from.
fn sims_here(m: Messages) -> Bool {
    first_here(m) and status_of(session_of(m)).sims
}

/// Whether the window shows something beside the people: a person, a kept conversation, the
/// setup or the groups.
fn beside_people(m: Messages) -> Bool {
    if let s = session_of(m) {
        s.member != .none or s.contact != "" or s.held != "" or s.pane != ""
    } else { false }
}

/// Starts the person's window state, the first time they open Messages.
action start_session(m: Messages) -> Session (icon: .message_circle, label: "Start Messages") {
    applies session_of(m) == .none "Messages is open already"
    m.sessions.insert(Session())
}

/// The person's window state from the moment they open the app, and how their accounts stand.
opened action opened(m: Messages) {
    let s = if let held = session_of(m) { held } else { start_session(m) }
    refresh_state(m, s)
}

/// What the device last said of the person's accounts.
fn status_of(s: Option<Session>) -> Status {
    if let held = s {
        match json.decode<Status>(held.status) {
            .ok(said) => said,
            .err(why) => Status(),
        }
    } else { Status() }
}

fn account_status(st: Status, service: Text, account: Text) -> AccountStatus {
    st.accounts.find(a -> a.service == service and a.account == account)
        ?? AccountStatus(service: service, account: account)
}

/// What a person reads of an account's state.
fn state_words(state: Text) -> Text {
    if state == "unconfigured" {
        "Add this service account in Settings to connect it."
    } else if state == "unavailable" {
        "This account's messaging service is unavailable. Check its setup in Settings."
    } else if state == "linking" {
        "Linking this account… Confirm the request on your phone."
    } else if state == "locked" {
        "Unlock this account to use messaging."
    } else if state == "offline" {
        "This account is offline. Your conversation will resume when it reconnects."
    } else if state == "ready" {
        "Account ready"
    } else if state == "needs-relink" {
        "Link this account again to resume messaging."
    } else if state == "failed" {
        "This account's connection stopped. Check its connection and account setup."
    } else { "Connection status is unavailable." }
}

/// What a person reads of a send the service refused, by its word.
fn refusal_words(word: Text) -> Text {
    if word == "uncertain" {
        "The outcome could not be confirmed. Check this conversation before sending again."
    } else if word == "reserved" {
        "This send was already attempted. Check its status in the conversation."
    } else if word == "invalid" {
        "Enter a message and select a current conversation."
    } else if word == "revoked" {
        "This service address has conflicting contact associations. Review them in Contacts."
    } else if word == "not-own-device" {
        "This computer is not one of your own Tilo devices, so it sends from none of your accounts. Send from a device of your own."
    } else if word == "" { "" } else { "Messaging connection unavailable" }
}

/// What a person reads of a delivery state.
fn delivery_of(word: Text) -> Text {
    if word == "submitted" {
        "Submitted"
    } else if word == "acknowledged" {
        "Accepted by service"
    } else if word == "delivered" {
        "Delivered to recipient"
    } else if word == "read" {
        "Read by recipient"
    } else if word == "uncertain" { "Send status uncertain" } else { "" }
}

// ---------------------------------------------------------------------------------------------
// The device's view of the accounts
/// Asks the device how the person's accounts stand, and keeps what it said when it changed:
/// the window's clock runs this while it shows.
action refresh_state(m: Messages, state: Session) {
    let s = state
    let listed = service_accounts().map(a -> ServiceAccount(service: a.service, account: a.id))
    let first: List<ServiceAccount> = match json.decode<Review>(s.first_review) {
        .ok(r) => if r.account != "" {
            [ServiceAccount(service: r.service, account: r.account)]
        } else { [] },
        .err(why) => [],
    }
    let routes: List<Route> = []
    if let c = choice_of(s.choice) {
        if c.service != "tilo" and c.recipient != "" {
            routes = [route_of(s.contact, c)]
        }
    }
    match engine.call("state", json.encode(StateAsk(accounts: listed + first, routes: routes))) {
        .ok(said) => {
            if said != s.status {
                s.status = said
            }
        }
        .err(why) => {
            let none = json.encode(Status())
            if s.status != none {
                s.status = none
            }
        }
    }
}

/// Every messaging account and how it stands: what an agent reads before it writes.
action list_account_states(m: Messages) -> Text (
    icon: .list,
    label: "List messaging account states",
) {
    let s = if let held = session_of(m) { held } else { start_session(m) }
    refresh_state(m, s)
    s.status
}

// ---------------------------------------------------------------------------------------------
// People and the way to write to them
/// Opens the conversation with a workspace member, on Tilo.
action open_member(m: Messages, who: Actor) (icon: .message_circle, label: "Open person") {
    let s = if let held = session_of(m) { held } else { start_session(m) }
    let c = member_choice(who, invoker.principal())
    s.member = .some(who)
    s.contact = ""
    s.held = ""
    s.pane = ""
    s.choice = choice_key(c)
    s.pages = []
    s.refused = ""
}

/// Opens the conversation with a person of the address book, on the first way of writing to
/// them that reaches them.
action open_person(m: Messages, person: Text) (icon: .message_circle, label: "Open person") {
    require contacts.directory.people.any(p -> p.id() == person and not p.removed) "This contact is unavailable."
    let s = if let held = session_of(m) { held } else { start_session(m) }
    if s.contact == person and s.pane == "" and s.held == "" {
        return
    }
    s.member = .none
    s.contact = person
    s.held = ""
    s.pane = ""
    s.refused = ""
    let listed = choices_of(contacts.directory, person)
    // A draft of a way of writing the directory no longer confirms goes with it.
    for gone in m.drafts.filter(
        x -> x.person == person and not listed.any(c -> choice_key(c) == x.route),
    ) {
        m.drafts.remove(gone)
    }
    let reaching = listed.find(c -> c.recipient != "") ?? listed.first()
    s.choice = if let c = reaching { choice_key(c) } else { "" }
    s.pages = []
    if session_route(m) != .none { read_history(m) }
}

/// Writes to the open person another way: a service, an account and an address of the choices
/// the window lists. One the directory no longer confirms is refused.
action select_route(m: Messages, choice: Text) (icon: .route, label: "Choose a route") {
    require choice_of(choice) != .none "This service address has conflicting contact associations. Review them in Contacts."
    require choices_of(contacts.directory, open_contact(m)).any(c -> choice_key(c) == choice) "This contact is unavailable."
    applies open_contact(m) != "" "Choose a person"
    let s = if let held = session_of(m) { held } else { start_session(m) }
    s.choice = choice
    s.pages = []
    s.refused = ""
    if session_route(m) != .none { read_history(m) }
}

/// Back to the list of people.
action back(m: Messages) (icon: .arrow_left, label: "Back to people") {
    applies beside_people(m) "The window shows the people already"
    let s = if let held = session_of(m) { held } else { start_session(m) }
    s.member = .none
    s.contact = ""
    s.held = ""
    s.pane = ""
    s.choice = ""
    s.pages = []
}

/// Whom the window can write to: the workspace's members first, then the address book's people.
action list_people(m: Messages) -> List<Text> (icon: .users, label: "List people") {
    let d = contacts.directory
    members_of(invoker).map(p -> "{p.id()}\t{p.name()}")
        + contacts_of(d).map(p -> "{p.id()}\t{p.label}")
}

/// The ways of writing to a person of the address book, each as the window keeps it.
action list_routes(m: Messages, person: Text) -> List<Text> (
    icon: .route,
    label: "List messaging choices",
) {
    choices_of(contacts.directory, person).map(c -> choice_key(c))
}

/// Confirms the address a choice names as the open person's, on the account it is written from:
/// the directory's `confirm-account`. Only a confirmation the person made binds an address.
action confirm_account(m: Messages, person: Text, choice: Text) (
    icon: .check,
    label: "Review account",
    did: "linked an account to a contact",
) {
    require confirmable(choice) "A verified service identity is required before this contact can be connected."
    require sender_current(choice) "Not configured"
    require contacts.directory.people.any(p -> p.id() == person and not p.removed) "This contact is unavailable."
    if let chosen = choice_of(choice) {
        contacts.confirm_account(
            contacts.directory,
            person,
            chosen.association,
            chosen.service,
            chosen.scope,
            chosen.recipient,
            chosen.revision,
        )
    }
    if open_contact(m) == person { select_route(m, choice) }
}

// ---------------------------------------------------------------------------------------------
// History and sending
/// The route the window writes on now, when it writes through the device.
/// The way of writing the window has open, while the directory still confirms it: a revoked
/// address, a changed identity or a disconnected account retires it, and nothing stands in.
fn held_choice(s: Session) -> Option<Choice> {
    if s.contact != ""
        and choices_of(contacts.directory, s.contact).any(c -> choice_key(c) == s.choice) {
        choice_of(s.choice)
    } else { .none }
}

fn current_route(s: Session) -> Option<Route> {
    if let c = held_choice(s) {
        if c.service != "tilo" and c.recipient != "" and c.account != "" and s.contact != "" {
            return .some(route_of(s.contact, c))
        }
    }
    .none
}

/// Reads the newest page of the open conversation again.
action read_history(m: Messages) (icon: .refresh, label: "Refresh messages") {
    applies session_route(m) != .none "Choose a service and account before writing a message."
    let s = if let held = session_of(m) { held } else { start_session(m) }
    if let route = current_route(s) {
        match engine.start("history", json.encode(HistoryAsk(route: route, before: ""))) {
            .ok(job) => {
                s.pages = [job]
            }
            .err(why) => {
                s.pages = []
                s.refused = "unavailable"
            }
        }
    }
}

/// Reads the page before the oldest one shown.
action read_older_history(m: Messages) (icon: .history, label: "Load older messages") {
    require oldest_shown(m) != "" "This is the start of the conversation."
    applies session_route(m) != .none "Choose a service and account before writing a message."
    let s = if let held = session_of(m) { held } else { start_session(m) }
    if let route = session_route(m) {
        match engine.start(
            "history",
            json.encode(HistoryAsk(route: route, before: oldest_shown(m))),
        ) {
            .ok(job) => {
                s.pages = s.pages + [job]
            }
            .err(why) => {
                s.refused = "unavailable"
            }
        }
    }
}

/// The messages the window's pages hold, oldest first, each once.
fn pages_of(s: Session) -> List<Line> {
    let out: List<Line> = []
    for job in s.pages {
        if let j = engine.job(job) {
            if j.state == .done {
                match json.decode<Page>(j.detail) {
                    .ok(page) => {
                        let fresh = page.messages.filter(l -> not out.any(o -> o.key == l.key))
                        out = fresh + out
                    }
                    .err(why) => {}
                }
            }
        }
    }
    out
}

/// Whether the oldest page read says older messages remain.
fn more_before(s: Session) -> Bool {
    if let job = s.pages.last() {
        if let j = engine.job(job) {
            if j.state == .done {
                match json.decode<Page>(j.detail) {
                    .ok(page) => {
                        return page.more
                    }
                    .err(why) => {}
                }
            }
        }
    }
    false
}

/// Why the newest page did not come, by the service's word; empty while it reads or once it did.
fn history_refusal(s: Session) -> Text {
    if let job = s.pages.first() {
        if let j = engine.job(job) {
            if j.state == .failed {
                return j.error
            }
        }
    }
    ""
}

fn reading(s: Session) -> Bool {
    s.pages.any(
        job -> if let j = engine.job(job) {
            j.state == .queued or j.state == .running
        } else { false },
    )
}

/// The draft of `route`, the person's own.
fn draft_of(m: Messages, route: Text) -> Option<Draft> {
    m.drafts.find(d -> d.route == route)
}

/// Keeps a draft for the way of writing the window has open, so the composer has words to bind.
action keep_draft(m: Messages, state: Session, route: Text, person: Text) -> Draft (
    icon: .pencil,
    label: "Keep a draft",
) {
    draft_of(m, route) ?? m.drafts.insert(Draft(route: route, person: person, body: RichText("")))
}

/// Throws away the words kept for a way of writing.
action drop_draft(m: Messages, d: Draft) (icon: .trash, label: "Drop a draft") {
    m.drafts.remove(d)
}

/// Sends what the draft holds on the way the window has open: to a member through Chat, to
/// anyone else through the device's messaging service, as one operation the service never sends
/// twice. The words stay in the draft until the service took them.
action send(m: Messages, d: Draft) uses random, notify (
    icon: .send,
    label: "Send message",
    did: "sent a message",
) {
    require not d.body.is_empty() "Enter a message and select a current conversation."
    require not sending(m) "This send was already attempted. Check its status in the conversation."
    require writes_somewhere(m) "Choose a service and account before writing a message."
    let s = if let held = session_of(m) { held } else { start_session(m) }
    if let who = s.member {
        chat.write_to(chat.conversations, who, d.body)
        d.body = RichText("")
    } else if let route = current_route(s) {
        s.operation = random.id()
        s.refused = ""
        match engine.start(
            leaves: true,
            "send",
            json.encode(SendAsk(route: route, operation: s.operation, text: d.body.text)),
            done: j -> sent(m, d, j),
        ) {
            .ok(job) => {
                s.send_job = job
            }
            .err(why) => {
                s.refused = "unavailable"
            }
        }
    }
}

/// Whether `path`, as a start answered it, is the job `j` says it is.
fn same_job(path: Text, j: host.Job) -> Bool {
    path != "" and (path == j.id or path.ends_with("/{j.id}"))
}

/// A send ended: the words leave the draft once the service took them, and the conversation is
/// read again; a refusal stays in the window in the service's words.
action sent(m: Messages, d: Draft, j: host.Job) (
    icon: .check,
    label: "Remove the delivered draft",
) {
    let s = if let held = session_of(m) { held } else { start_session(m) }
    if not same_job(s.send_job, j) {
        return
    }
    s.send_job = ""
    if j.state == .done {
        d.body = RichText("")
        s.refused = ""
        if session_route(m) != .none { read_history(m) }
    } else if j.state == .failed {
        s.refused = if j.error == "" { "unavailable" } else { j.error }
    }
}

// ---------------------------------------------------------------------------------------------
// Conversations agents and automations hand to Messages
/// The key a conversation is kept by: a phone number's digits (with its `+`), or anything else
/// in lowercase words joined by dashes, at most 64 characters. Empty for words that name no one.
fn conversation_key(party: Text) -> Text {
    let trimmed = party.trim()
    let bare = if trimmed.starts_with("tilo.messages/conversations/") {
        trimmed.slice(28, trimmed.len()).trim()
    } else { trimmed }
    let cs = bare.split("").filter(c -> c != "")
    let phone = cs.any(c -> is_digit(c)) and cs.all(c -> is_digit(c) or " +-().".contains(c))
    let key = if phone {
        let digits = cs.filter(c -> is_digit(c)).join("")
        if bare.starts_with("+") { "+{digits}" } else { digits }
    } else {
        let slug = ""
        for c in bare.lower().split("").filter(c -> c != "") {
            if is_digit(c) or c == "_" or c.lower() != c.upper() or c >= "぀" {
                slug = slug + c
            } else if slug != "" and not slug.ends_with("-") {
                slug = slug + "-"
            }
        }
        slug
    }
    let short = key.slice(0, key.len().min(64))
    let clean = if short.ends_with("-") { short.slice(0, short.len() - 1) } else { short }
    if clean == "+" { "" } else { clean }
}

fn conversation_named(m: Messages, key: Text) -> Option<Conversation> {
    m.conversations.find(c -> c.key == key)
}

fn check_message(from: Text, to: Text, body: Text) -> Text {
    if from.len() > 256 or to.len() > 256 {
        "a sender or recipient is at most 256 characters"
    } else if body.len() > 16384 { "a message is at most 16384 characters" } else { "" }
}

/// Keeps a message `from` someone sent, in the conversation it names or theirs. The same
/// message delivered twice is kept once. Answers the conversation's key.
action deliver_message(
    m: Messages,
    from: Text,
    body: Text,
    to: Text = "",
    date: Text = "",
    conversation: Text = "",
) -> Text uses time (icon: .inbox, label: "Deliver message", did: "delivered a message") {
    require from.trim() != "" "Name who sent it"
    require body.trim() != "" "A message says something"
    require check_message(from.trim(), to.trim(), body) == "" check_message(
        from.trim(),
        to.trim(),
        body,
    )
    require delivered_at(date, now) != .none "`{date}` is not a date: write 2026-10-11T09:30:00Z"
    require conversation_key(named_or(conversation, from)) != "" "`{named_or(conversation, from)}` names no one a conversation can be with"
    let key = conversation_key(named_or(conversation, from))
    let at = delivered_at(date, now) ?? now
    let held = conversation_named(m, key)
        ?? m.conversations.insert(Conversation(with: from.trim(), key: key), key: key)
    let id = "m" + std.sha256("{key}\t{from.trim()}\t{to.trim()}\t{at.iso()}\t{body}").slice(0, 32)
    if not held.messages.any(x -> x.mark == id) {
        // Kept in the order the messages were sent, whatever order they arrive in.
        let later = held.messages.find(x -> x.at > at)
        let made = held.messages.insert(
            Message(mark: id, from: from.trim(), to: to.trim(), body: body, at: at),
            key: id,
        )
        if let next = later { held.messages.move(made, before: next) }
    }
    key
}

/// Queues a message to someone, in the conversation it names or theirs, for a device that holds
/// the account to send. Answers the conversation's key.
action send_message(m: Messages, to: Text, body: Text, conversation: Text = "") -> Text uses time (
    icon: .send,
    label: "Send message",
    did: "queued a message",
) {
    require conversation_key(named_or(conversation, to)) != "" "`{to}` names no one to write to"
    require body.trim() != "" "A message says something"
    require check_message(to.trim(), "", body) == "" check_message(to.trim(), "", body)
    let key = conversation_key(named_or(conversation, to))
    let held = conversation_named(m, key)
        ?? m.conversations.insert(Conversation(with: to.trim(), key: key), key: key)
    held.messages.insert(Message(to: held.with, body: body, at: now, outgoing: true, queued: true))
    key
}

/// Queues a reply in a kept conversation.
action reply(m: Messages, conversation: Text, body: Text) -> Text uses time (
    icon: .reply,
    label: "Reply",
    did: "queued a reply",
) {
    require conversation_key(conversation) != "" "`{conversation}` is not a conversation id"
    require body.trim() != "" "A message says something"
    require check_message("", "", body) == "" check_message("", "", body)
    require conversation_named(m, conversation_key(conversation)) != .none "no conversation `{conversation}`: `send_message` starts one"
    let key = conversation_key(conversation)
    if let held = conversation_named(m, key) {
        held.messages.insert(
            Message(to: held.with, body: body, at: now, outgoing: true, queued: true),
        )
    }
    key
}

/// Opens a kept conversation.
action open_conversation(m: Messages, c: Conversation) (
    icon: .message_circle,
    label: "Open conversation",
) {
    let s = if let held = session_of(m) { held } else { start_session(m) }
    s.member = .none
    s.contact = ""
    s.choice = ""
    s.pane = ""
    s.pages = []
    s.held = c.key
}

fn found_of(c: Conversation, x: Message) -> Found {
    Found(
        conversation: c.key,
        with: c.with,
        from: x.from,
        to: x.to,
        body: x.body,
        date: x.at.iso(),
        at: x.at,
        outgoing: x.outgoing,
        queued: x.queued,
    )
}

/// The kept messages holding every word of `query`, newest first, at most 200.
action search_messages(m: Messages, query: Text) -> List<Message> uses time (
    icon: .search,
    label: "Search messages",
) {
    require not words_of(query).is_empty() "search for some words"
    let words = words_of(query)
    let hits: List<Message> = []
    for c in m.conversations {
        for x in c.messages {
            let hay = "{c.with} {c.key} {x.from} {x.to} {x.body}".lower()
            if words.all(w -> hay.contains(w)) {
                hits = hits + [x]
            }
        }
    }
    hits.sort_by(x -> (now - x.at).seconds()).take(200)
}

/// The words of `query`, in lowercase.
fn words_of(query: Text) -> List<Text> {
    query.lower().replace("\t", " ").replace("\n", " ").split(" ").filter(w -> w != "")
}

fn chosen(m: Messages, conversation: Text) -> List<Conversation> {
    if conversation.trim() == "" {
        m.conversations.map(c -> c)
    } else {
        let key = conversation_key(conversation)
        m.conversations.filter(c -> c.key == key)
    }
}

fn markdown_of(list: List<Conversation>) -> Text {
    list
        .map(
            c -> "# {if c.with == "" { c.key } else { c.with }}\n\n"
                + c.messages
                    .map(
                        x -> "- **{if x.outgoing { "You" } else { x.from }}** ({x.at.iso()}){if x.queued { " (queued)" } else { "" }}: {x.body.replace("\n", "\n  ")}\n",
                    )
                    .join("")
                + "\n",
        )
        .join("")
}

/// The kept conversations, or the one named, as Markdown.
action export_markdown(m: Messages) -> File (icon: .download, export: "Markdown") {
    File.of_text("messages.md", markdown_of(chosen(m, "")))
}

/// The kept conversations, or the one named, as Markdown (`md`) or JSON (`json`).
action export_content(m: Messages, format: Text = "md", conversation: Text = "") -> File (
    icon: .download,
    label: "Export conversations",
) {
    require conversation.trim() == "" or not chosen(m, conversation).is_empty() "no conversation `{conversation}`"
    require format_known(format) "`{format}` is not a format: md or json"
    let list = chosen(m, conversation)
    if format.trim() == "json" {
        File.of_text(
            "messages.json",
            json.encode(list.flat_map(c -> c.messages.map(x -> found_of(c, x)))),
        )
    } else {
        File.of_text("messages.md", markdown_of(list))
    }
}

// ---------------------------------------------------------------------------------------------
// Account setup
/// Opens the account setup in the content column.
action open_account_setup(m: Messages) (icon: .settings, label: "Accounts") {
    let s = if let held = session_of(m) { held } else { start_session(m) }
    s.pane = "setup"
    refresh_state(m, s)
    s.member = .none
    s.contact = ""
    s.held = ""
    s.choice = ""
    s.pages = []
}

/// Chooses which of the person's accounts this device links.
action select_setup_account(m: Messages, service: Text, account: Text) (
    icon: .user_check,
    label: "Choose account",
) {
    require service_accounts().any(a -> a.id == account and a.service == service) "Private linking is unavailable for this account on this device."
    require linking_idle(m) "This account already has a linking attempt. Finish or close it before starting another."
    applies pane_is(m, "setup") "Open the account setup first"
    let s = if let held = session_of(m) { held } else { start_session(m) }
    s.setup_service = service
    s.setup_account = account
    s.linked_job = ""
}

/// Asks the service for the private code a phone scans to link the chosen account here.
action begin_link(m: Messages) (icon: .qr_code, label: "Link this device") {
    require setup_chosen(m) "Select the service and account to link on this device."
    require linking_idle(m) "This account already has a linking attempt. Finish or close it before starting another."
    applies pane_is(m, "setup") "Open the account setup first"
    let s = if let held = session_of(m) { held } else { start_session(m) }
    match engine.start(
        outside: true,
        "link_begin",
        json.encode(ServiceAccount(service: s.setup_service, account: s.setup_account)),
    ) {
        .ok(job) => {
            s.link_job = job
            s.linked_job = ""
        }
        .err(why) => {
            s.refused = "unavailable"
        }
    }
}

/// Finishes linking once the phone scanned the code.
action finish_link(m: Messages) (icon: .check, label: "Finish linking") {
    applies pane_is(m, "setup") "Open the account setup first"
    applies link_code(m) != .none "Linking stopped. Return to account selection to start a new attempt."
    let s = if let held = session_of(m) { held } else { start_session(m) }
    if let code = link_code(m) {
        match engine.start(
            outside: true,
            "link_finish",
            json.encode(
                FinishAsk(
                    service: s.setup_service,
                    account: s.setup_account,
                    attempt: code.attempt,
                ),
            ),
        ) {
            .ok(job) => {
                s.linked_job = job
                s.link_job = ""
            }
            .err(why) => {
                s.refused = "unavailable"
            }
        }
    }
}

/// Closes the setup, stopping a linking attempt that was not finished.
action cancel_link(m: Messages) (icon: .x, label: "Close setup") {
    applies pane_is(m, "setup") "Open the account setup first"
    let s = if let held = session_of(m) { held } else { start_session(m) }
    if s.link_job != "" {
        engine.cancel(s.link_job)
        engine.call(
            "setup_cancel",
            json.encode(ServiceAccount(service: s.setup_service, account: s.setup_account)),
        )
    }
    s.link_job = ""
    s.linked_job = ""
    s.setup_service = ""
    s.setup_account = ""
    s.pane = ""
}

/// The private code a job brought, while it is the one to scan.
fn linking_of(job: Text) -> Option<Linking> {
    if let j = engine.job(job) {
        if j.state == .done {
            match json.decode<Linking>(j.detail) {
                .ok(code) => {
                    if code.code != "" {
                        return .some(code)
                    }
                }
                .err(why) => {}
            }
        }
    }
    .none
}

/// Starts a first account on `service`: WhatsApp asks first for its protected storage to be
/// reviewed; Signal shows its code at once.
action begin_first_account(m: Messages, service: Text) (
    icon: .plus,
    label: "Set up a private Messages account",
) {
    require service == "whatsapp" or service == "signal" "Private linking is unavailable for this account on this device."
    require first_idle(m) "This account already has a linking attempt. Finish or close it before starting another."
    applies first_here(m) "Messaging isn't running here, so no account can be linked."
    let s = if let held = session_of(m) { held } else { start_session(m) }
    s.first_service = service
    s.first_reviewed = false
    let op = if service == "whatsapp" and s.first_review == "" {
        "first_prepare"
    } else { "first_begin" }
    match engine.start(
        outside: true,
        op,
        json.encode(ServiceAsk(service: service)),
        done: j -> first_moved(m, s, op, j),
    ) {
        .ok(job) => {
            s.first_job = job
        }
        .err(why) => {
            s.refused = "unavailable"
        }
    }
}

/// Finishes linking the first account once its code was scanned.
action finish_first_account(m: Messages) (icon: .check, label: "Finish linking") {
    applies pane_is(m, "setup") "Open the account setup first"
    applies first_code(m) != .none "Linking stopped. Return to account selection to start a new attempt."
    let s = if let held = session_of(m) { held } else { start_session(m) }
    if let c = first_code(m) {
        match engine.start(
            outside: true,
            "first_finish",
            json.encode(FinishAsk(service: c.service, account: c.account, attempt: c.attempt)),
            done: j -> first_moved(m, s, "first_finish", j),
        ) {
            .ok(job) => {
                s.first_job = job
            }
            .err(why) => {
                s.refused = "unavailable"
            }
        }
    }
}

/// Confirms what the device asked to be reviewed: WhatsApp's protected storage, then the
/// identity the service reports.
action review_first_account(m: Messages) (icon: .check, label: "Confirm identity") {
    applies pane_is(m, "setup") "Open the account setup first"
    applies first_reviewing(m) != .none "Linking stopped. Return to account selection to start a new attempt."
    let s = if let held = session_of(m) { held } else { start_session(m) }
    if let r = first_reviewing(m) {
        match engine.start(
            outside: true,
            "first_review",
            json.encode(
                ReviewAsk(
                    service: r.service,
                    account: r.account,
                    display_name: service_name(r.service),
                ),
            ),
            done: j -> first_moved(m, s, "first_review", j),
        ) {
            .ok(job) => {
                s.first_job = job
                s.first_reviewed = true
            }
            .err(why) => {
                s.refused = "unavailable"
            }
        }
    }
}

/// Lists the device's SIMs an SMS account can use.
action list_sims(m: Messages) (icon: .smartphone, label: "Choose an SMS SIM") {
    require first_idle(m) "This account already has a linking attempt. Finish or close it before starting another."
    applies sims_here(m) "This device has no SIM to send SMS from."
    let s = if let held = session_of(m) { held } else { start_session(m) }
    match engine.start("first_sims", "\{\}") {
        .ok(job) => {
            s.sims_job = job
        }
        .err(why) => {
            s.refused = "unavailable"
        }
    }
}

/// Uses one of the SIMs listed for an SMS account.
action select_sim(m: Messages, selection: Text) (icon: .smartphone, label: "Use this SIM") {
    applies sims_here(m) "This device has no SIM to send SMS from."
    let s = if let held = session_of(m) { held } else { start_session(m) }
    s.first_service = "sms"
    s.first_reviewed = false
    match engine.start(
        outside: true,
        "first_select_sim",
        json.encode(SimAsk(selection: selection)),
        done: j -> first_moved(m, s, "first_select_sim", j),
    ) {
        .ok(job) => {
            s.first_job = job
            s.sims_job = ""
        }
        .err(why) => {
            s.refused = "unavailable"
        }
    }
}

/// A step of the first account's setup ended: what it asks to review is kept, and a failure
/// leaves the setup interrupted.
action first_moved(m: Messages, state: Session, op: Text, j: host.Job) {
    let s = state
    if not same_job(s.first_job, j) {
        return
    }
    if j.state == .done {
        if op == "first_prepare" or op == "first_finish" or op == "first_select_sim" {
            s.first_review = j.detail
            s.first_reviewed = false
        }
        refresh_state(m, s)
    }
}

/// Back to choosing a service after a setup stopped: the device forgets the attempt.
action reset_first_account(m: Messages) (icon: .rotate_ccw, label: "Return to account selection") {
    applies first_here(m) "Messaging isn't running here, so no account can be linked."
    let s = if let held = session_of(m) { held } else { start_session(m) }
    if let r = first_review_of(s) {
        engine.call(
            "first_cancel",
            json.encode(ServiceAccount(service: r.service, account: r.account)),
        )
    }
    if s.sims_job != "" { engine.call("sims_cancel", "\{\}") }
    if s.first_job != "" { engine.cancel(s.first_job) }
    s.first_job = ""
    s.first_review = ""
    s.first_reviewed = false
    s.first_service = ""
    s.sims_job = ""
}

fn first_review_of(s: Session) -> Option<Review> {
    if s.first_review == "" {
        return .none
    }
    match json.decode<Review>(s.first_review) {
        .ok(r) => .some(r),
        .err(why) => .none,
    }
}

fn first_busy(s: Session) -> Bool {
    if let j = engine.job(s.first_job) {
        j.state == .queued or j.state == .running
    } else { false }
}

/// The first account's phase, as the device said it.
fn first_phase(s: Session) -> Text {
    if let r = first_review_of(s) {
        account_status(status_of(.some(s)), r.service, r.account).phase
    } else { "empty" }
}

fn first_failed(s: Session) -> Bool {
    if let j = engine.job(s.first_job) { j.state == .failed } else { false }
}

// ---------------------------------------------------------------------------------------------
// Groups
/// Opens the private groups of the person's WhatsApp accounts.
action list_groups(m: Messages) (icon: .users, label: "List private groups") {
    let s = if let held = session_of(m) { held } else { start_session(m) }
    s.pane = "groups"
    s.member = .none
    s.contact = ""
    s.held = ""
    s.choice = ""
    s.pages = []
    s.group = ""
    let st = status_of(.some(s))
    let jobs: List<Text> = []
    for a in service_accounts() {
        if account_status(st, a.service, a.id).groups {
            match engine.start(
                "groups",
                json.encode(ServiceAccount(service: a.service, account: a.id)),
            ) {
                .ok(job) => {
                    jobs = jobs + [job]
                }
                .err(why) => {}
            }
        }
    }
    s.groups_jobs = jobs
}

/// Closes the groups.
action close_groups(m: Messages) (icon: .x, label: "Close groups") {
    applies pane_is(m, "groups") "Open the private groups first"
    let s = if let held = session_of(m) { held } else { start_session(m) }
    s.pane = ""
    s.group = ""
    s.groups_jobs = []
}

/// Opens one group: who is in it, and what was said there.
action read_group_membership(m: Messages, service: Text, account: Text, group: Text) (
    icon: .users,
    label: "Read group membership",
) {
    applies pane_is(m, "groups") "Open the private groups first"
    let s = if let held = session_of(m) { held } else { start_session(m) }
    s.group_service = service
    s.group_account = account
    s.group = group
    let ask = json.encode(GroupAsk(service: service, account: account, group: group))
    match engine.start("group", ask) {
        .ok(job) => {
            s.group_job = job
        }
        .err(why) => {
            s.refused = "unavailable"
        }
    }
    match engine.start("group_history", ask) {
        .ok(job) => {
            s.group_history_job = job
        }
        .err(why) => {}
    }
}

/// Reads the open group's messages again.
action read_group_history(m: Messages) (icon: .refresh, label: "Read group messages") {
    require group_open(m) "Group membership is unavailable for this account."
    applies pane_is(m, "groups") "Open the private groups first"
    let s = if let held = session_of(m) { held } else { start_session(m) }
    read_group_membership(m, s.group_service, s.group_account, s.group)
}

fn membership_of(s: Session) -> Option<Membership> {
    if let j = engine.job(s.group_change_job) {
        if j.state == .done {
            match json.decode<GroupChanged>(j.detail) {
                .ok(changed) => {
                    if let held = changed.membership {
                        return .some(held)
                    }
                }
                .err(why) => {}
            }
        }
    }
    if let j = engine.job(s.group_job) {
        if j.state == .done {
            match json.decode<Membership>(j.detail) {
                .ok(said) => {
                    return .some(said)
                }
                .err(why) => {}
            }
        }
    }
    .none
}

fn group_lines(s: Session) -> List<Line> {
    let j = engine.job(s.group_history_job)
    if let done = j {
        if done.state == .done {
            match json.decode<GroupHistory>(done.detail) {
                .ok(said) => {
                    return said.messages
                }
                .err(why) => {}
            }
        }
    }
    []
}

fn is_admin(g: Membership) -> Bool {
    g.members.any(x -> x.address == g.self_member and x.role != "member")
}

/// Sends what the group's draft holds to the open group.
action send_to_group(m: Messages, d: Draft) uses random (
    icon: .send,
    label: "Send to this group",
    did: "sent to a group",
) {
    require open_membership(m) != .none "Group membership is unavailable for this account."
    require not d.body.is_empty() "Enter a message and select a current conversation."
    require may_write(m) "Group membership is unavailable for this account."
    let s = if let held = session_of(m) { held } else { start_session(m) }
    if let g = open_membership(m) {
        match engine.start(
            leaves: true,
            "group_send",
            json.encode(
                GroupSendAsk(
                    service: s.group_service,
                    account: s.group_account,
                    group: s.group,
                    review: g.review,
                    operation: random.id(),
                    text: d.body.text,
                ),
            ),
            done: j -> group_sent(m, d, j),
        ) {
            .ok(job) => {
                s.group_history_job = job
            }
            .err(why) => {
                s.refused = "unavailable"
            }
        }
    }
}

action group_sent(m: Messages, d: Draft, j: host.Job) {
    let s = if let held = session_of(m) { held } else { start_session(m) }
    if j.state == .done {
        d.body = RichText("")
        read_group_membership(m, s.group_service, s.group_account, s.group)
    } else if j.state == .failed {
        s.refused = j.error
    }
}

/// Changes who is in the open group: adds a reviewed contact by a route to them, removes a
/// member, or leaves.
action change_group(m: Messages, change: Text, member: Text = "", choice: Text = "") uses random (
    icon: .user_plus,
    label: "Add reviewed contact",
) {
    require open_membership(m) != .none "Group membership is unavailable for this account."
    require change == "add" or change == "remove" or change == "leave" "no group change is called `{change}`"
    require change != "remove" or may_remove(m, member) "Group membership is unavailable for this account."
    require change != "add" or person_of_choice(choice) != "" "A verified service identity is required before this contact can be connected."
    applies pane_is(m, "groups") "Open the private groups first"
    let s = if let held = session_of(m) { held } else { start_session(m) }
    let route: Option<Route> = if change == "add" {
        if let c = choice_of(choice) {
            .some(route_of(person_of_choice(choice), c))
        } else { .none }
    } else { .none }
    if let g = open_membership(m) {
        match engine.start(
            leaves: true,
            "group_change",
            json.encode(
                GroupChangeAsk(
                    service: s.group_service,
                    account: s.group_account,
                    group: s.group,
                    review: g.review,
                    operation: random.id(),
                    change: change,
                    member: member,
                    route: route,
                ),
            ),
        ) {
            .ok(job) => {
                s.group_change_job = job
                if change == "leave" {
                    s.group = ""
                    s.group_job = ""
                    s.group_history_job = ""
                }
            }
            .err(why) => {
                s.refused = "unavailable"
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The window
fn role_words(role: Text) -> Text {
    if role == "admin" {
        "Administrator"
    } else if role == "super-admin" { "Group creator" } else { "Member" }
}

/// The words a draft holds, as a person's row shows them.
/// Only a draft of a way of writing that still reaches them: one of a retired route is never
/// offered again.
fn draft_line(m: Messages, person: Text, routes: List<Text>) -> Option<Text> {
    if let d = m.drafts.find(
            x -> x.person == person and not x.body.is_empty() and routes.contains(x.route),
        ) {
        .some("Draft: {d.body.excerpt}")
    } else { .none }
}

/// The window: the people the person writes to, and the conversation, the setup or the groups
/// beside them.
view messages_window(m: Messages) {
    let s = session_of(m)
    let d = contacts.directory
    let st = status_of(s)
    let members = members_of(invoker)
    let people = contacts_of(d)
    let selected = if let held = s {
        if let who = held.member {
            "member:{who.id()}"
        } else if held.contact != "" {
            "contact:{held.contact}"
        } else if held.held != "" {
            "held:{held.held}"
        } else if held.pane != "" { held.pane } else { "" }
    } else { "" }
    if let held = s {
        every 5s: refresh_state(m, held)
    }
    ui.NavShell(content_kind: .page, title: "Messages", sidebar_title: "Messages") {
        actions: {
            ui.SearchButton()
        }
        sidebar: {
            ui.NavList(selected: selected) {
                for who in members {
                    ui.NavItem(
                        "member:{who.id()}",
                        who.name(),
                        person: who,
                        secondary: draft_line(
                            m,
                            who.id(),
                            [choice_key(member_choice(who, invoker))],
                        ),
                    ) {
                        on select: open_member(m, who)
                    }
                }
                for p in people {
                    ui.NavItem(
                        "contact:{p.id()}",
                        p.label,
                        icon: .user,
                        secondary: draft_line(
                            m,
                            p.id(),
                            choices_of(d, p.id()).map(c -> choice_key(c)),
                        ),
                    ) {
                        on select: open_person(m, p.id())
                    }
                }
                for c in m.conversations {
                    ui.NavItem(
                        "held:{c.key}",
                        c.with,
                        icon: .inbox,
                        count: c.messages.count(x -> x.queued),
                    ) {
                        on select: open_conversation(m, c)
                    }
                }
                ui.NavItem("setup", "Accounts", icon: .settings) {
                    on select: open_account_setup(m)
                }
                if st.accounts.any(a -> a.groups) {
                    ui.NavItem("groups", "Private groups", icon: .users) {
                        on select: list_groups(m)
                    }
                }
            }
        }
        content: {
            if let held = s {
                if held.pane == "setup" {
                    setup(m, held, st)
                } else if held.pane == "groups" {
                    groups(m, held, st)
                } else if held.held != "" {
                    if let c = conversation_named(m, held.held) {
                        kept(c)
                    } else {
                        ui.EmptyState("This conversation is no longer here.", icon: .message_circle)
                    }
                } else if let who = held.member {
                    tilo_conversation(m, held, who)
                } else if held.contact != "" {
                    conversation(m, held, st)
                } else {
                    nobody(m, members, people)
                }
            } else {
                nobody(m, members, people)
            }
        }
    }
}

/// What the content column says before a person is chosen: whom to choose, or where to add
/// people when there is nobody.
view nobody(m: Messages, members: List<Actor>, people: List<contacts.Person>) {
    if members.is_empty() and people.is_empty() and m.conversations.is_empty() {
        ui.EmptyState(
            "No conversations yet",
            icon: .message_circle,
            message: "Add people in Contacts to start a conversation.",
            action: "Open Contacts",
        ) {
            on act: open_contacts(m)
        }
    } else {
        ui.EmptyState(
            "Choose a person",
            icon: .message_circle,
            message: "Your conversations stay separate for each service and account.",
        )
    }
}

/// The conversation with a workspace member: theirs in Chat, written to from here.
view tilo_conversation(m: Messages, s: Session, who: Actor) {
    let route = s.choice
    let said = if let dm = direct_with(who, invoker) { dm.messages.map(x -> x) } else { [] }
    stack(fill, gap: .none) {
        ui.Docked {
            foot: {
                composer_of(m, s, route, who.id(), "Message {who.name()}", true)
            }
            ui.ConversationList(name: "Conversation with {who.name()}") {
                for x in said {
                    ui.ConversationEntry(
                        x,
                        x.body.text,
                        author: if created(x)?.who == .some(invoker) {
                            .none
                        } else { .some(who.name()) },
                        time: created(x)?.at,
                        outgoing: created(x)?.who == .some(invoker),
                    )
                }
            }
        }
    }
}

/// The person's direct conversation in Chat with `who`, none while they have none.
fn direct_with(who: Actor, me: Actor) -> Option<chat.Direct> {
    for item in chat.conversations.channels {
        match item {
            c: chat.Category => {}
            ch: chat.TextChannel => {}
            v: chat.VoiceChannel => {}
            dm: chat.Direct => {
                if dm.people.len() == 2 and dm.people.contains(who) and dm.people.contains(me) {
                    return .some(dm)
                }
            }
        }
    }
    .none
}

/// The foot of a conversation: the draft of the way it is written, or a button that keeps one.
view composer_of(
    m: Messages,
    s: Session,
    route: Text,
    person: Text,
    placeholder: Text,
    ready: Bool,
) {
    if ready and route != "" {
        if let d = draft_of(m, route) {
            std.composer_draft(d, placeholder, send(m, d))
        } else {
            ui.Button("Write a message", icon: .pencil) {
                on press: keep_draft(m, s, route, person)
            }
        }
    }
}

/// The conversation with a person of the address book, on the way the window writes to them.
view conversation(m: Messages, s: Session, st: Status) {
    let d = contacts.directory
    let person = d.people.find(p -> p.id() == s.contact and not p.removed)
    if let p = person {
        let listed = choices_of(d, p.id())
        let c = held_choice(s)
        let lines = if c != .none { pages_of(s) } else { [] }
        let refusal = history_refusal(s)
        let account_state = if let chosen = c {
            account_status(st, chosen.service, chosen.account).state
        } else { "unknown" }
        stack(fill, gap: .none) {
            row(gap: .related, align: .center) {
                ui.Text(p.label, role: .heading)
                spacer()
                ui.Dropdown(
                    bind: s.choice,
                    options: listed.map(x -> choice_label(x)),
                    values: listed.map(x -> choice_key(x)),
                    caption: "Message using",
                ) {
                    on change(v: Text): select_route(m, v)
                }
                ui.Button("Refresh messages", icon: .refresh, icon_only: true) {
                    on press: read_history(m)
                }
            }
            if c == .none and s.choice != "" {
                ui.Banner(
                    "This way of writing to {p.label} is no longer confirmed.",
                    message: "Choose another way above, or review the address in Contacts.",
                    kind: .warning,
                )
            }
            if let chosen = c {
                if chosen.account == "" {
                    ui.Banner("Not configured", message: state_words("unconfigured"), kind: .info)
                } else if chosen.recipient == "" {
                    ui.Banner(
                        "Confirm a service address for this person.",
                        message: "Verify this address belongs to the person above. Similar names or phone numbers are not proof.",
                        kind: .warning,
                    )
                } else if account_state != "ready" {
                    ui.Banner(state_words(account_state), kind: .warning)
                }
            }
            if refusal != "" {
                ui.Banner(
                    if refusal == "revoked" {
                        "This service address has conflicting contact associations. Review them in Contacts."
                    } else {
                        "This service's message history is unavailable."
                    },
                    kind: .warning,
                )
            }
            if s.refused != "" {
                ui.Banner(refusal_words(s.refused), kind: .error)
            }
            ui.Docked {
                foot: {
                    composer_of(
                        m,
                        s,
                        s.choice,
                        p.id(),
                        "Write a message",
                        if let chosen = c {
                            chosen.recipient != "" and chosen.account != ""
                        } else { false },
                    )
                }
                stack(fill) {
                    if c == .none {
                    } else if reading(s) and lines.is_empty() {
                        ui.Text("Loading messages…", role: .caption, secondary: true)
                    } else if more_before(s) {
                        ui.Button("Load older messages", icon: .history, role: .plain) {
                            on press: read_older_history(m)
                        }
                    } else if not lines.is_empty() {
                        ui.Text(
                            "This is the start of the conversation.",
                            role: .caption,
                            secondary: true,
                        )
                    } else if refusal == "" and not s.pages.is_empty() {
                        ui.Text("No messages on this service yet", role: .caption, secondary: true)
                    }
                    ui.ConversationList(name: "Conversation with {p.label}") {
                        for l in lines {
                            ui.ConversationEntry(
                                said: l.text,
                                id: l.key,
                                author: if l.incoming { .some(p.label) } else { .none },
                                time: l.at,
                                outgoing: not l.incoming,
                                delivery: if l.delivery == "submitted" {
                                    .submitted
                                } else if l.delivery == "acknowledged" {
                                    .sent
                                } else if l.delivery == "delivered" {
                                    .delivered
                                } else if l.delivery == "read" {
                                    .read
                                } else if l.delivery == "uncertain" { .uncertain },
                            )
                        }
                    }
                }
            }
        }
    } else {
        ui.EmptyState("This contact is unavailable.", icon: .user_x)
    }
}

/// A conversation kept in this copy: what was delivered into it and what waits to be sent.
view kept(c: Conversation) {
    stack(fill) {
        ui.Text(c.with, role: .heading)
        ui.Banner(
            "Kept in this copy of Messages. Words written here wait for a device that holds the account to send them.",
            kind: .info,
        )
        ui.ConversationList(name: "Conversation with {c.with}") {
            for x in c.messages {
                ui.ConversationEntry(
                    x,
                    x.body,
                    author: if x.outgoing { .none } else { .some(x.from) },
                    time: x.at,
                    outgoing: x.outgoing,
                    delivery: if x.queued { .queued },
                )
            }
        }
    }
}

/// The accounts and their setup: link this device to an account, or set up a first one.
view setup(m: Messages, s: Session, st: Status) {
    let listed = service_accounts()
    let link = linking_of(s.link_job)
    let linked = if let j = engine.job(s.linked_job) { j.state == .done } else { false }
    let link_failed = if let j = engine.job(s.link_job) { j.state == .failed } else { false }
    std.page(m) {
        ui.Text("Accounts", role: .title)
        if not listed.is_empty() {
            ui.Section("Your accounts") {
                for a in listed {
                    let said = account_status(st, a.service, a.id)
                    ui.ListRow(
                        service_name(a.service),
                        secondary: "{account_name(a.id)} · {state_words(said.state)}",
                        selected: s.setup_account == a.id,
                    ) {
                        on activate: select_setup_account(m, a.service, a.id)
                    }
                }
            }
        }
        if s.setup_account != "" {
            let said = account_status(st, s.setup_service, s.setup_account)
            if linked {
                ui.Text(
                    "Device linked. Messaging resumes after its connection and inbox are ready.",
                )
            } else if link_failed {
                ui.Text("Linking stopped. Return to account selection to start a new attempt.")
            } else if let code = link {
                ui.Text(
                    "Scan this private code from Linked devices on your phone, then finish linking.",
                )
                private_code(code.code)
                ui.Button("Finish linking", icon: .check, role: .primary) {
                    on press: finish_link(m)
                }
            } else if not said.linkable {
                ui.Text("Private linking is unavailable for this account on this device.")
            } else {
                ui.Text("Select the service and account to link on this device.", secondary: true)
                ui.Button("Link this device", icon: .qr_code, role: .primary) {
                    on press: begin_link(m)
                }
            }
        }
        first_account(m, s, st)
        ui.Button("Close setup", icon: .x, role: .plain) {
            on press: cancel_link(m)
        }
    }
}

/// A private code to scan: never in a capture or a shared screen.
view private_code(code: Text) {
    protect capture
    ui.QrCode(code, alt: "Private device linking code for the selected account", secret: true)
}

/// Setting up a first private account: WhatsApp or Signal by a code the phone scans, SMS by a
/// SIM of this device.
view first_account(m: Messages, s: Session, st: Status) {
    let phase = first_phase(s)
    let review = first_review_of(s)
    let code = linking_of(s.first_job)
    let busy = first_busy(s)
    if first_failed(s) or phase == "interrupted" {
        ui.Text("Linking stopped. Return to account selection to start a new attempt.")
        ui.Button("Return to account selection", icon: .rotate_ccw) {
            on press: reset_first_account(m)
        }
    } else if let c = code {
        ui.Text("Scan this private code from Linked devices on your phone, then finish linking.")
        private_code(c.code)
        if c.service == "whatsapp" {
            ui.Button("Refresh private linking code", icon: .refresh) {
                on press: begin_first_account(m, "whatsapp")
            }
        }
        ui.Button("Finish linking", icon: .check, role: .primary) {
            on press: finish_first_account(m)
        }
    } else if phase == "ready" {
        ui.Text("Device linked. Messaging resumes after its connection and inbox are ready.")
    } else if let r = review {
        let storage = r.service == "whatsapp" and r.number == ""
        ui.Text(
            if storage {
                "Review access to WhatsApp's private encrypted storage, then confirm the identity WhatsApp reports. Text messages and private group text work, including membership changes you review. Communities, disappearing messages, media and calls don't work."
            } else {
                "Verify the selected service identity: {if r.number == "" { r.scope } else { r.number }}"
            },
        )
        if r.service == "whatsapp" and phase == "linking" {
            ui.Button("Show private linking code", icon: .qr_code) {
                on press: begin_first_account(m, "whatsapp")
            }
        } else if not s.first_reviewed
            or r.service == "whatsapp" and r.number != "" and phase == "review" {
            ui.Button(
                if storage { "Review protected storage access" } else { "Confirm identity" },
                icon: .check,
                role: .primary,
            ) {
                on press: review_first_account(m)
            }
        }
    } else {
        if let j = engine.job(s.sims_job) {
            if j.state == .done {
                match json.decode<Sims>(j.detail) {
                    .ok(sims) => {
                        if sims.choices.is_empty() {
                            ui.Text("No available SIMs were found on this device.")
                        }
                        for sim in sims.choices {
                            ui.Button("Use {sim.label}", icon: .smartphone) {
                                on press: select_sim(m, sim.selection)
                            }
                        }
                    }
                    .err(why) => {}
                }
            }
        }
        ui.Section("Link an account") {
            if not st.first {
                ui.Text(
                    "Messaging isn't running here, so no account can be linked. Link WhatsApp or Signal on a Tilo computer instead.",
                )
            } else {
                ui.ListRow(
                    "Link a WhatsApp account",
                    secondary: "Scan a code with WhatsApp on your phone",
                    icon: .phone,
                ) {
                    on activate: begin_first_account(m, "whatsapp")
                }
                ui.ListRow(
                    "Link a Signal account",
                    secondary: "Scan a code with Signal on your phone",
                    icon: .lock,
                ) {
                    on activate: begin_first_account(m, "signal")
                }
                if st.sims {
                    ui.ListRow("Choose an SMS SIM", icon: .smartphone) {
                        on activate: list_sims(m)
                    }
                }
            }
        }
    }
}

/// The private groups of the person's WhatsApp accounts, and the one open.
view groups(m: Messages, s: Session, st: Status) {
    std.page(m) {
        row(gap: .related, align: .center) {
            ui.Text("Private groups", role: .title)
            spacer()
            ui.Button("Close groups", icon: .x, role: .plain) {
                on press: close_groups(m)
            }
        }
        ui.Text(
            "These groups belong to the selected WhatsApp account, separately from workspace members.",
            role: .caption,
            secondary: true,
        )
        for job in s.groups_jobs {
            if let j = engine.job(job) {
                if j.state == .failed {
                    ui.Text("Group membership is unavailable for this account.")
                } else if j.state == .done {
                    match json.decode<Groups>(j.detail) {
                        .ok(listed) => {
                            ui.Section("WhatsApp groups for {account_name(listed.account)}") {
                                if listed.groups.is_empty() {
                                    ui.Text("No participating groups.", secondary: true)
                                }
                                for g in listed.groups {
                                    ui.ListRow(
                                        if g.title == "" { "Untitled group" } else { g.title },
                                        selected: s.group == g.key,
                                    ) {
                                        on activate: read_group_membership(
                                            m,
                                            listed.service,
                                            listed.account,
                                            g.key,
                                        )
                                    }
                                }
                            }
                        }
                        .err(why) => {}
                    }
                }
            }
        }
        if s.group != "" {
            group(m, s)
        }
    }
}

/// One group: who is in it with their roles, what was said, and the changes the person may make.
view group(m: Messages, s: Session) {
    let d = contacts.directory
    if let g = membership_of(s) {
        let admin = is_admin(g)
        let key = "group:{g.service}:{g.account}:{g.group}"
        ui.Section(if g.title == "" { "Untitled group" } else { g.title }) {
            for x in g.members {
                row(gap: .related, align: .center) {
                    ui.Text(x.address)
                    ui.Badge(role_words(x.role))
                    spacer()
                    if admin and x.address != g.self_member {
                        ui.Button("Remove {x.address}", icon: .user_minus, role: .plain) {
                            on press: change_group(m, "remove", member: x.address)
                        }
                    }
                }
            }
            row(gap: .related) {
                ui.Button("Refresh membership", icon: .refresh) {
                    on press: read_group_history(m)
                }
                ui.Button("Leave this group", icon: .log_out, role: .destructive) {
                    on press: change_group(m, "leave")
                }
            }
        }
        if admin {
            ui.Section("Add reviewed contact") {
                for p in contacts_of(d) {
                    for c in choices_of(d, p.id())
                        .filter(
                            c -> c.service == g.service
                                and c.account == g.account
                                and c.recipient != "",
                        ) {
                        ui.Button(
                            "Add {p.label} using {choice_label(c)}",
                            icon: .user_plus,
                            role: .plain,
                        ) {
                            on press: change_group(m, "add", choice: choice_key(c))
                        }
                    }
                }
            }
        }
        ui.ConversationList(name: if g.title == "" { "Untitled group" } else { g.title }) {
            for l in group_lines(s) {
                ui.ConversationEntry(
                    said: l.text,
                    id: l.key,
                    author: if l.incoming { .some(l.author) } else { .none },
                    time: l.at,
                    outgoing: not l.incoming,
                    delivery: if l.delivery == "submitted" {
                        .submitted
                    } else if l.delivery == "acknowledged" {
                        .sent
                    } else if l.delivery == "delivered" {
                        .delivered
                    } else if l.delivery == "read" {
                        .read
                    } else if l.delivery == "uncertain" { .uncertain },
                )
            }
        }
        if not g.announcement or admin {
            if let d = draft_of(m, key) {
                std.composer_draft(d, "Send to this group", send_to_group(m, d))
            } else {
                ui.Button("Write a message", icon: .pencil) {
                    on press: keep_draft(m, s, key, key)
                }
            }
        }
    } else {
        ui.Text("Loading messages…", role: .caption, secondary: true)
    }
}

/// The address book's person the window has open, by the directory's id; empty for none.
fn open_contact(m: Messages) -> Text {
    if let s = session_of(m) { s.contact } else { "" }
}

/// Whether `choice` names an address confirmed for its person, from an enabled account of its
/// service under the scope it is confirmed in.
fn confirmable(choice: Text) -> Bool {
    if let c = choice_of(choice) {
        c.account != "" and c.association != "" and c.scope != "" and c.revision != ""
    } else { false }
}

fn sender_current(choice: Text) -> Bool {
    if let c = choice_of(choice) {
        service_accounts()
            .any(
                a -> a.id == c.account and a.service == c.service and a.scope == c.scope,
            )
    } else { false }
}

fn session_route(m: Messages) -> Option<Route> {
    if let s = session_of(m) { current_route(s) } else { .none }
}

/// The oldest message the window shows, by its id; empty for none.
fn oldest_shown(m: Messages) -> Text {
    if let s = session_of(m) {
        if let first = pages_of(s).first() {
            return first.key
        }
    }
    ""
}

fn sending(m: Messages) -> Bool {
    if let s = session_of(m) { s.send_job != "" } else { false }
}

/// Whether the window has someone to write to and a way that reaches them.
fn writes_somewhere(m: Messages) -> Bool {
    if let s = session_of(m) {
        s.member != .none or current_route(s) != .none
    } else { false }
}

fn is_digit(c: Text) -> Bool {
    c >= "0" and c <= "9"
}

/// When a message delivered at `date` was sent: now when it says none.
fn delivered_at(date: Text, at: Time) -> Option<Time> {
    if date.trim() == "" { .some(at) } else { Time.parse(date.trim(), near: at) }
}

fn named_or(conversation: Text, party: Text) -> Text {
    if conversation.trim() == "" { party } else { conversation }
}

fn format_known(format: Text) -> Bool {
    ["md", "markdown", "json", ""].contains(format.trim())
}

fn linking_idle(m: Messages) -> Bool {
    if let s = session_of(m) { s.link_job == "" } else { true }
}

fn setup_chosen(m: Messages) -> Bool {
    if let s = session_of(m) { s.setup_account != "" } else { false }
}

fn link_code(m: Messages) -> Option<Linking> {
    if let s = session_of(m) { linking_of(s.link_job) } else { .none }
}

fn first_idle(m: Messages) -> Bool {
    if let s = session_of(m) { not first_busy(s) } else { true }
}

fn first_code(m: Messages) -> Option<Linking> {
    if let s = session_of(m) { linking_of(s.first_job) } else { .none }
}

fn first_reviewing(m: Messages) -> Option<Review> {
    if let s = session_of(m) { first_review_of(s) } else { .none }
}

fn group_open(m: Messages) -> Bool {
    if let s = session_of(m) { s.group != "" } else { false }
}

fn open_membership(m: Messages) -> Option<Membership> {
    if let s = session_of(m) { membership_of(s) } else { .none }
}

/// Whether the person may write in the open group: a member of it, where only administrators
/// write in an announcement group.
fn may_write(m: Messages) -> Bool {
    if let g = open_membership(m) { not g.announcement or is_admin(g) } else { false }
}

/// Whether the person may remove `member` from the open group: an administrator, never
/// themselves.
fn may_remove(m: Messages, member: Text) -> Bool {
    if let g = open_membership(m) { is_admin(g) and member != g.self_member } else { false }
}

/// The address book's person a choice reaches, by the directory's id; empty for none.
fn person_of_choice(choice: Text) -> Text {
    let d = contacts.directory
    if let p = d.people.find(
            p -> not p.removed and choices_of(d, p.id()).any(x -> choice_key(x) == choice),
        ) {
        p.id()
    } else { "" }
}
