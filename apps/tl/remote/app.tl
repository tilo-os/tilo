app so.tilo.remote "Remote Desktop" (
    icon: .monitor_smartphone,
    category: .network,
    tagline: "Shows and controls another desktop",
    keywords: ["remote desktop", "rdp", "vnc", "ssh", "screen sharing", "remote access", "remote control", "remmina", "krdc", "remote desktop connection", "mstsc", "terminal", "fernzugriff", "fernwartung", "remotedesktop", "entfernter zugriff", "bildschirmfreigabe", "fernsteuerung", "verbinden"],
    splash_budget: 250,
    web: ["*"],
)

/// What a saved connection speaks.
enum ConnectionKind {
    /// Secure shell: a terminal on the machine.
    ssh
    /// Virtual Network Computing: the machine's screen, driven from here.
    vnc
    /// Remote Desktop Protocol: kept and edited with the others, but Remote opens none.
    rdp
}

/// How a connection signs in. The secret itself is never here: a password or a key's passphrase
/// is kept in the workspace's vault.
enum AuthMethod {
    password
    public_key
}

/// One saved machine: where it is and how to reach it. Everyone the connections are shared with
/// sees the same list; a password is kept in the workspace's vault, never in the connection.
type Connection (title: name, untitled: "Untitled connection") {
    name: Text
    /// The host name or address.
    host: Text
    port: Int = 22
    user: Text
    kind: ConnectionKind = .ssh
    auth: AuthMethod = .password
    /// When a session to it was last opened, from any device.
    last_used: Option<Time> = .none
    /// A word the connection is filed under.
    group: Text
    /// Pinned to the top of the list.
    favorite: Bool = false

    search {
        filter favorite (icon: .pin)
        filter kind
        sort name a_z
    }
}

/// The workspace's saved connections.
type Remote {
    connections: Collection<Connection>
}

root remote: Remote

default view window for Remote

/// A session this device opened: the stream that carries it and what it is to. It is the device's
/// own, so a member's sessions never show on another member's screen.
type Session (title: name, untitled: "Session") {
    /// The saved connection, by its id.
    connection: Text
    name: Text
    kind: ConnectionKind = .ssh
    /// The stream the device runs for it, until it is closed.
    job: Text
    /// The terminal an SSH shell is drawn in.
    terminal: Text
    /// Why it never started, in words the window shows.
    trouble: Text
}

/// What one machine last did when this device reached for it.
type Seen {
    connection: Text
    online: Bool = false
}

/// An SSH key this device trusted for one machine, after its person checked it.
type TrustedKey {
    host: Text
    port: Int
    algorithm: Text
    fingerprint: Text
}

/// What this device keeps of its own: the sessions it opened, what it saw of each machine and the
/// machines' keys it trusts.
type Here {
    sessions: Collection<Session>
    seen: Collection<Seen>
    trusted: Collection<TrustedKey>
}

device root here: Here

// ---------------------------------------------------------------- what the device says of a stream
/// An SSH key as a machine showed it.
type KeyId {
    algorithm: Text = ""
    fingerprint: Text = ""
}

/// What a stream says of itself, as `engine.state` reads it.
type Said {
    /// `connecting`, `connected`, `closed`, `failed`, `need-trust` after a first contact, or
    /// `key-changed`.
    phase: Text = ""
    /// Why it failed, in the engine's words.
    why: Text = ""
    /// The key a first contact showed.
    algorithm: Text = ""
    fingerprint: Text = ""
    /// The key this device trusted, and the one the machine showed instead.
    expected: Option<KeyId> = .none
    observed: Option<KeyId> = .none
}

type SshAsk {
    host: Text
    port: Int
    user: Text
    /// `password` or `key`.
    method: Text
    /// The key the password or the key's passphrase is kept under in the vault.
    secret_key: Text
    expected: Option<KeyId>
    columns: Int
    rows: Int
}

type VncAsk {
    host: Text
    port: Int
    /// The key the server's password is kept under in the vault.
    password_key: Text
}

type KeepAsk {
    key: Text
    secret: Text
}

type Started {
    job: Text = ""
    /// The terminal an SSH stream's shell is drawn in.
    session: Text = ""
}

type SendAsk {
    job: Text
    text: Text
}

type TransferAsk {
    job: Text
    local_path: Text
    remote_path: Text
}

// ---------------------------------------------------------------- what a connection is
fn kind_word(kind: ConnectionKind) -> Text {
    if kind == .ssh { "SSH" } else if kind == .vnc { "VNC" } else { "RDP" }
}

fn auth_word(auth: AuthMethod) -> Text {
    if auth == .password { "Password" } else { "Public key" }
}

fn default_port(kind: ConnectionKind) -> Int {
    if kind == .ssh { 22 } else if kind == .vnc { 5900 } else { 3389 }
}

/// The sign-in methods a protocol takes, so the form never offers one its transport ignores.
fn methods_of(kind: ConnectionKind) -> List<AuthMethod> {
    if kind == .ssh { [.password, .public_key] } else { [.password] }
}

/// How a protocol signs in unless the person picks another way.
fn first_method(kind: ConnectionKind) -> AuthMethod {
    methods_of(kind).first() ?? .password
}

fn port_fits(port: Int) -> Bool {
    port >= 1 and port <= 65535
}

/// What a connection's secret is for. It is short: with the connection's id it names the vault
/// item, which takes a purpose of one short line.
fn secret_purpose(kind: ConnectionKind, auth: AuthMethod) -> Text {
    if kind == .vnc { "vnc" } else if auth == .public_key { "passphrase" } else { "password" }
}

/// Where a connection goes, as a row says it: `user@host:port`, the port left out where it is the
/// protocol's own.
fn address(c: Connection) -> Text {
    let at = if c.port == 0 or c.port == default_port(c.kind) {
        c.host
    } else { "{c.host}:{c.port}" }
    if c.kind == .ssh and c.user != "" { "{c.user}@{at}" } else { at }
}

/// Why a connection cannot be saved or opened as it stands, in words the form shows; empty when
/// it can.
fn name_problem(name: Text) -> Text {
    if name.trim() == "" { "A connection needs a name" } else { "" }
}

fn host_problem(host: Text) -> Text {
    if host.trim() == "" { "A connection needs an address" } else { "" }
}

fn port_problem(port: Text) -> Text {
    if let n = port.trim().to_int() {
        if n >= 1 and n <= 65535 { "" } else { "A port is 1–65535" }
    } else { "A port is 1–65535" }
}

fn auth_problem(kind: ConnectionKind, auth: AuthMethod) -> Text {
    if methods_of(kind).contains(auth) {
        ""
    } else if kind == .ssh {
        "SSH signs in with a password or a key"
    } else if kind == .vnc {
        "VNC signs in with a password"
    } else { "RDP signs in with a password" }
}

/// Whether the form, as typed, has a field the person must change before it saves.
fn form_problem(
    name: Text,
    host: Text,
    port: Text,
    kind: ConnectionKind,
    auth: AuthMethod,
) -> Bool {
    name_problem(name) != ""
        or host_problem(host) != ""
        or port_problem(port) != ""
        or auth_problem(kind, auth) != ""
}

/// Why a saved connection cannot be opened, or empty when it can.
fn profile_problem(c: Connection) -> Text {
    let auth = auth_problem(c.kind, c.auth)
    if auth != "" { auth } else { host_problem(c.host) }
}

/// What a row says on its trailing edge when the connection cannot be opened as it stands.
fn row_meta(c: Connection) -> Text {
    if c.kind == .rdp {
        "RDP isn't supported"
    } else if profile_problem(c) != "" { "Needs editing" } else { "" }
}

// ---------------------------------------------------------------- keeping the connections
/// Saves a machine to reach over SSH or VNC.
primary action add_connection(
    r: Remote,
    name: Text,
    host: Text,
    kind: ConnectionKind = .ssh,
    port: Option<Int> = .none,
    user: Option<Text> = .none,
    auth: Option<AuthMethod> = .none,
    group: Text = "",
) -> Connection (icon: .plus, label: "Add connection", did: "added a connection") {
    require name_problem(name) == "" name_problem(name)
    require host_problem(host) == "" host_problem(host)
    require port_fits(port ?? default_port(kind)) "A port is 1–65535"
    require auth_problem(kind, auth ?? first_method(kind)) == "" auth_problem(
        kind,
        auth ?? first_method(kind),
    )
    let p = port ?? default_port(kind)
    let method = auth ?? first_method(kind)
    r.connections.insert(
        Connection(
            name: name.trim(),
            host: host.trim(),
            port: p,
            user: if kind == .ssh { (user ?? "").trim() } else { "" },
            kind: kind,
            auth: method,
            group: group.trim(),
        ),
    )
}

/// Changes what a saved connection is: only what differs is written, so two people editing one
/// connection keep each other's changes.
action change_connection(
    c: Connection,
    name: Text,
    host: Text,
    kind: ConnectionKind,
    port: Int,
    user: Text,
    auth: AuthMethod,
    group: Text,
) (icon: .pencil, label: "Change connection", did: "changed a connection") {
    require name_problem(name) == "" name_problem(name)
    require host_problem(host) == "" host_problem(host)
    require port >= 1 and port <= 65535 "A port is 1–65535"
    require auth_problem(kind, auth) == "" auth_problem(kind, auth)
    if c.name != name.trim() {
        c.name = name.trim()
    }
    if c.host != host.trim() {
        c.host = host.trim()
    }
    if c.kind != kind {
        c.kind = kind
    }
    if c.port != port {
        c.port = port
    }
    let u = if kind == .ssh { user.trim() } else { "" }
    if c.user != u {
        c.user = u
    }
    if c.auth != auth {
        c.auth = auth
    }
    if c.group != group.trim() {
        c.group = group.trim()
    }
}

/// Pins a connection to the top of the list, or unpins it.
action pin(c: Connection, pinned: Bool = not c.favorite) (
    icon: .pin,
    on: c.favorite,
    label: "Pin",
) {
    c.favorite = pinned
}

/// Deletes a saved connection for everyone it is shared with; the machine itself is not changed.
/// A password kept for it stays in the workspace's vault until it is forgotten in Passwords.
destructive action delete_connection(r: Remote, c: Connection) (
    icon: .trash,
    label: "Delete connection",
    did: "deleted a connection",
) {
    for s in here.sessions.filter(s -> s.connection == c.id) {
        close_session(s)
    }
    r.connections.remove(c)
}

/// The key the secret `c` signs in with is kept under in the vault, the way `c` signs in now: a
/// password typed for a password sign-in is never handed over as a key's passphrase.
fn secret_key(c: Connection) -> Text {
    "{c.id}:{secret_purpose(c.kind, c.auth)}"
}

/// Keeps the password or the key's passphrase `c` signs in with in the workspace's vault, as
/// this app's own item. Search never holds it, and no shared record carries it.
action keep_secret(c: Connection, secret: Text) (
    icon: .key,
    label: "Keep the password in the vault",
) {
    match engine.call("keep-secret", json.encode(KeepAsk(key: secret_key(c), secret: secret))) {
        .ok(_) => {}
        .err(why) => { notice(why, kind: .error) }
    }
}

/// Notes that a session to `c` was opened now.
action mark_used(c: Connection) uses time (label: "Set its last use") {
    c.last_used = .some(now)
}

/// Why this device opens no session to `c` at all, or empty when it can try.
fn unreachable(c: Connection) -> Text {
    if c.kind == .rdp {
        "Remote opens SSH and VNC, but not RDP. Choose SSH or VNC to keep using this machine."
    } else { "" }
}

/// The key this device trusted for the machine `c` reaches.
fn trusted_key(c: Connection) -> Option<TrustedKey> {
    here.trusted.find(k -> k.host == c.host and k.port == c.port)
}

// ---------------------------------------------------------------- sessions
/// Opens a live session to a saved connection: a terminal for SSH, the machine's screen for VNC.
/// Nothing is sent before the saved address and sign-in are checked again.
action connect(c: Connection) -> Session uses time (
    icon: .plug,
    label: "Connect",
    did: "connected to a machine",
) {
    require profile_problem(c) == "" profile_problem(c)
    for old in here.sessions.filter(s -> s.connection == c.id) {
        close_session(old)
    }
    c.last_used = .some(now)
    let s = here.sessions.insert(Session(connection: c.id, name: c.name, kind: c.kind))
    if unreachable(c) != "" {
        s.trouble = unreachable(c)
        return s
    }
    let started = if c.kind == .ssh {
        let expected: Option<KeyId> = if let k = trusted_key(c) {
            .some(KeyId(algorithm: k.algorithm, fingerprint: k.fingerprint))
        } else { .none }
        engine.stream(
            "ssh",
            json.encode(
                SshAsk(
                    host: c.host,
                    port: c.port,
                    user: c.user,
                    method: if c.auth == .public_key { "key" } else { "password" },
                    secret_key: secret_key(c),
                    expected: expected,
                    columns: 80,
                    rows: 24,
                ),
            ),
        )
    } else {
        engine.stream(
            "vnc",
            json.encode(VncAsk(host: c.host, port: c.port, password_key: secret_key(c))),
        )
    }
    match started {
        .ok(text) => {
            match json.decode<Started>(text) {
                .ok(answer) => {
                    s.job = answer.job
                    s.terminal = answer.session
                }
                .err(why) => {
                    s.trouble = why
                }
            }
        }
        .err(why) => {
            s.trouble = why
        }
    }
    s
}

/// Ends a session; the saved connection stays.
action close_session(s: Session) (icon: .unlink, label: "Disconnect") {
    if s.job != "" { engine.cancel(s.job) }
    here.sessions.remove(s)
}

/// Ends the session to `c` this device has open.
action disconnect(c: Connection) (icon: .unlink, label: "Disconnect") {
    for s in here.sessions.filter(s -> s.connection == c.id) {
        close_session(s)
    }
}

/// What a session's stream says of itself now.
fn said(s: Session) -> Said {
    if s.job == "" {
        return Said(phase: "failed", why: s.trouble)
    }
    match json.decode<Said>(engine.state(s.job)) {
        .ok(word) => if word.phase == "" { Said(phase: "connecting") } else { word },
        .err(_) => Said(phase: "connecting"),
    }
}

/// Notes what this device saw of the machine a session reached: up once it answered, down once
/// it failed to.
action observe(s: Session) {
    let word = said(s)
    let online = word.phase == "connected"
    if word.phase != "connected" and word.phase != "failed" {
        return
    }
    if let seen = here.seen.find(x -> x.connection == s.connection) {
        if seen.online != online {
            seen.online = online
        }
    } else {
        here.seen.insert(Seen(connection: s.connection, online: online))
    }
}

/// The SSH session this device has open to `c`.
fn ssh_session(c: Connection) -> Option<Session> {
    here.sessions.find(x -> x.connection == c.id and x.kind == .ssh)
}

/// The key the session to `c` showed on a first contact, or an empty one.
fn shown_key(c: Connection) -> KeyId {
    if let s = session_of(c) {
        let word = said(s)
        if word.phase == "need-trust" {
            return KeyId(algorithm: word.algorithm, fingerprint: word.fingerprint)
        }
    }
    KeyId()
}

/// Types `text` into an open SSH session, as if it were typed at its terminal.
action send_text(c: Connection, text: Text) (icon: .keyboard, label: "Send text") {
    require session_of(c) != .none "No session to this machine is open here"
    require ssh_session(c) != .none "Text goes to an SSH terminal"
    let job = ssh_session(c)?.job ?? ""
    match engine.call("ssh-send", json.encode(SendAsk(job: job, text: text))) {
        .ok(_) => {}
        .err(why) => { notice(why, kind: .error) }
    }
}

/// Sends one file from this device to the machine an SSH session is open to.
action put_file(c: Connection, local_path: Text, remote_path: Text) (
    icon: .upload,
    label: "Send file",
) {
    require ssh_session(c) != .none "No SSH session to this machine is open here"
    let job = ssh_session(c)?.job ?? ""
    match engine.start(
        leaves: true,
        "put",
        json.encode(TransferAsk(job: job, local_path: local_path, remote_path: remote_path)),
        done: j -> moved_file(c, j),
    ) {
        .ok(_) => { notice("Sending {local_path}") }
        .err(why) => { notice(why, kind: .error) }
    }
}

/// Fetches one file from the machine an SSH session is open to, onto this device.
action fetch_file(c: Connection, remote_path: Text, local_path: Text) (
    icon: .download,
    label: "Fetch file",
) {
    require ssh_session(c) != .none "No SSH session to this machine is open here"
    let job = ssh_session(c)?.job ?? ""
    match engine.start(
        "fetch",
        json.encode(TransferAsk(job: job, local_path: local_path, remote_path: remote_path)),
        done: j -> moved_file(c, j),
    ) {
        .ok(_) => { notice("Fetching {remote_path}") }
        .err(why) => { notice(why, kind: .error) }
    }
}

/// Says how a file's way to or from a machine ended.
action moved_file(c: Connection, j: host.Job) {
    if j.state == .done {
        notice("The file reached its place", kind: .success)
    } else if j.error != "" { notice(j.error, kind: .error) }
}

/// Trusts the SSH key a machine showed on first contact, on this device only; connecting again
/// signs in. The key is checked against the one shown: a machine that changed it since is asked
/// about again.
action trust_host_key(c: Connection, fingerprint: Text) (
    icon: .shield_check,
    label: "Trust key",
    did: "trusted a machine's key",
) {
    require session_of(c) != .none "No session to this machine is open here"
    require shown_key(c).fingerprint != "" "This machine shows no new key to trust"
    require shown_key(c).fingerprint == fingerprint "The machine's key changed before it was trusted. Review it again."
    require trusted_key(c) == .none "This device trusts another key for this machine. Forget it first."
    let shown = shown_key(c)
    here.trusted.insert(
        TrustedKey(
            host: c.host,
            port: c.port,
            algorithm: shown.algorithm,
            fingerprint: shown.fingerprint,
        ),
    )
}

/// Forgets the key this device trusted for a machine whose key changed; connecting again shows
/// the new one to check.
destructive action forget_host_key(c: Connection) (icon: .shield_alert, label: "Forget key") {
    require trusted_key(c) != .none "This device trusts no key for this machine"
    for k in here.trusted.filter(k -> k.host == c.host and k.port == c.port) {
        here.trusted.remove(k)
    }
    notice("This device no longer trusts the old key")
}

// ---------------------------------------------------------------- the window
/// The sessions open here, for the connection `c`.
fn session_of(c: Connection) -> Option<Session> {
    here.sessions.find(s -> s.connection == c.id)
}

fn connection_of(r: Remote, id: Text) -> Option<Connection> {
    r.connections.find(c -> c.id == id)
}

/// The machines the list shows: pinned first, then by name, narrowed by what was searched.
fn listed(r: Remote, place: Text, found: Text) -> List<Connection> {
    let asked = found.trim().lower()
    r.connections
        .filter(
            c -> (place != "pinned" or c.favorite)
                and (asked == ""
                    or c.name.lower().contains(asked)
                    or c.host.lower().contains(asked)
                    or c.group.lower().contains(asked)),
        )
        .sort_by(c -> c.name.lower())
}

fn online(c: Connection) -> Bool {
    here.seen.find(x -> x.connection == c.id)?.online ?? false
}

view window(r: Remote) {
    // Which place the rail shows: every connection, the pinned ones, or the session.
    state place: Text = "all"
    // The list, or the form that starts or changes a connection.
    state screen: Text = "list"
    state editing: Option<Connection> = .none
    // The form's fields; a password typed here goes to the workspace's vault on Save.
    state name: Text = ""
    state host: Text = ""
    state port: Text = "22"
    state user: Text = ""
    state group: Text = ""
    state secret: Text = ""
    state kind: ConnectionKind = .ssh
    state auth: AuthMethod = .password
    // Whether Save was pressed on what the form holds, so its problems show beside each field.
    state checking: Bool = false
    // The session the window shows, by its connection.
    state open: Text = ""
    state actual_size: Bool = false
    state full: Bool = false
    // A question the window asks before it acts: `connect`, `disconnect` or `trust`, about `asked`.
    state asking: Text = ""
    state asked: Text = ""
    state found: Text = ""
    on show new "New connection": set(editing, .none)
        .then(set(name, ""))
        .then(set(host, ""))
        .then(set(port, "22"))
        .then(set(user, ""))
        .then(set(group, ""))
        .then(set(secret, ""))
        .then(set(kind, .ssh))
        .then(set(auth, .password))
        .then(set(checking, false))
        .then(set(screen, "form"))
    on show(c: Connection): set(open, c.id).then(set(screen, "list")).then(set(place, "session"))
    let shown = connection_of(r, open)
    let session = if let c = shown { session_of(c) } else { .none }
    let rows = listed(r, place, found)
    ui.NavShell(
        content_kind: .list,
        title: window_title(screen, place, editing, shown),
        count: if screen == "list" and place != "session" { rows.len() } else { 0 },
        can_go_back: screen == "form",
    ) {
        on search(t: Text): set(found, t)
        on back: set(screen, "list")
        sidebar: {
            ui.NavList(rail: true, selected: place) {
                on select(key: Text): set(place, key).then(set(screen, "list"))
                ui.NavItem("all", "All", icon: .list)
                ui.NavItem("pinned", "Pinned", icon: .pin)
                if let c = shown {
                    ui.NavItem(
                        "session",
                        "Session",
                        icon: if c.kind == .ssh { .square_terminal } else { .screen_share },
                    )
                }
            }
        }
        actions: {
            if screen == "form" {
                ui.Button("Save", icon: .check, role: .primary, key: "Ctrl+S") {
                    on press: {
                        let saved = save_form(
                            r,
                            editing,
                            name,
                            host,
                            port,
                            user,
                            kind,
                            auth,
                            group,
                            secret,
                        )
                        if saved {
                            set(screen, "list").then(set(checking, false))
                        } else { set(checking, true) }
                    }
                }
            } else if place == "session" and session != .none {
                if let c = shown {
                    if c.kind != .ssh {
                        ui.Button(
                            if actual_size { "Fit to window" } else { "Actual size" },
                            icon: if actual_size { .minimize_2 } else { .square },
                        ) {
                            on press: set(actual_size, not actual_size)
                        }
                        ui.Button("Full screen", icon: .maximize_2, key: "F11") {
                            on press: set(full, true)
                        }
                    }
                    ui.Button("Disconnect", icon: .unlink) {
                        on press: set(asking, "disconnect").then(set(asked, c.id))
                    }
                }
            } else if not r.connections.is_empty() {
                ui.Button("New connection", icon: .plus, role: .primary, key: "Ctrl+N") {
                    on press: set(editing, .none)
                        .then(set(name, ""))
                        .then(set(host, ""))
                        .then(set(port, "22"))
                        .then(set(user, ""))
                        .then(set(group, ""))
                        .then(set(secret, ""))
                        .then(set(kind, .ssh))
                        .then(set(auth, .password))
                        .then(set(checking, false))
                        .then(set(screen, "form"))
                }
            }
        }
        content: {
            stack(fill) {
                if screen == "form" {
                    connection_form(
                        r,
                        editing,
                        name,
                        host,
                        port,
                        user,
                        group,
                        secret,
                        kind,
                        auth,
                        checking,
                        screen,
                    )
                } else if place == "session" {
                    if let c = shown {
                        if let s = session {
                            session_view(c, s, actual_size, full, asking, asked)
                        } else {
                            ui.EmptyState(
                                "Not connected",
                                icon: if c.kind == .ssh {
                                    .square_terminal
                                } else { .screen_share },
                                message: "No session to {c.name} is open.",
                                action: "Connect",
                            ) {
                                on act: set(asking, "connect").then(set(asked, c.id))
                            }
                        }
                    } else {
                        ui.EmptyState(
                            "This connection was deleted",
                            icon: .monitor,
                            message: "Someone you share your connections with removed it.",
                        )
                    }
                } else {
                    connection_list(
                        r,
                        rows,
                        place,
                        asked,
                        asking,
                        editing,
                        name,
                        host,
                        port,
                        user,
                        group,
                        secret,
                        kind,
                        auth,
                        checking,
                        screen,
                    )
                }
                confirmation(r, asking, asked, place, open)
            }
        }
    }
}

fn window_title(
    screen: Text,
    place: Text,
    editing: Option<Connection>,
    shown: Option<Connection>,
) -> Text {
    if screen == "form" {
        if editing == .none { "New connection" } else { "Edit connection" }
    } else if place == "session" {
        shown?.name ?? "Session"
    } else if place == "pinned" { "Pinned connections" } else { "Connections" }
}

/// Saves the form: adds the connection or changes the one it was opened over, and keeps the
/// password in the workspace's vault. Answers whether it saved; a refusal leaves the form as it was.
action save_form(
    r: Remote,
    editing: Option<Connection>,
    name: Text,
    host: Text,
    port: Text,
    user: Text,
    kind: ConnectionKind,
    auth: AuthMethod,
    group: Text,
    secret: Text,
) -> Bool {
    if form_problem(name, host, port, kind, auth) {
        return false
    }
    let p = port.trim().to_int() ?? default_port(kind)
    let c = if let was = editing {
        change_connection(was, name, host, kind, p, user, auth, group)
        was
    } else {
        add_connection(r, name, host, kind, .some(p), .some(user), .some(auth), group)
    }
    if secret != "" { keep_secret(c, secret) }
    true
}

/// The saved connections: the pinned ones first under their own heading, then the rest.
view connection_list(
    r: Remote,
    rows: List<Connection>,
    place: Text,
    asked: State<Text>,
    asking: State<Text>,
    editing: State<Option<Connection>>,
    name: State<Text>,
    host: State<Text>,
    port: State<Text>,
    user: State<Text>,
    group: State<Text>,
    secret: State<Text>,
    kind: State<ConnectionKind>,
    auth: State<AuthMethod>,
    checking: State<Bool>,
    screen: State<Text>,
) {
    if r.connections.is_empty() {
        ui.EmptyState(
            "No saved connections",
            icon: .monitor,
            message: "Save a machine to reach it over SSH or VNC.",
            action: "New connection",
        ) {
            on act: set(editing, .none)
                .then(set(name, ""))
                .then(set(host, ""))
                .then(set(port, "22"))
                .then(set(user, ""))
                .then(set(group, ""))
                .then(set(secret, ""))
                .then(set(kind, .ssh))
                .then(set(auth, .password))
                .then(set(checking, false))
                .then(set(screen, "form"))
        }
    } else if rows.is_empty() {
        if place == "pinned" {
            ui.EmptyState(
                "No pinned connections",
                icon: .pin,
                message: "Pin the machines you use most to keep them here.",
            )
        } else {
            ui.EmptyState("No connections match", icon: .search)
        }
    } else {
        let pinned = rows.filter(c -> c.favorite)
        let others = rows.filter(c -> not c.favorite)
        scroll {
            page {
                if place == "pinned" or pinned.is_empty() or others.is_empty() {
                    ui.Section() {
                        for c in rows {
                            connection_row(
                                c,
                                asked,
                                asking,
                                editing,
                                name,
                                host,
                                port,
                                user,
                                group,
                                secret,
                                kind,
                                auth,
                                checking,
                                screen,
                            )
                        }
                    }
                } else {
                    ui.Section("Pinned") {
                        for c in pinned {
                            connection_row(
                                c,
                                asked,
                                asking,
                                editing,
                                name,
                                host,
                                port,
                                user,
                                group,
                                secret,
                                kind,
                                auth,
                                checking,
                                screen,
                            )
                        }
                    }
                    ui.Section("Other connections") {
                        for c in others {
                            connection_row(
                                c,
                                asked,
                                asking,
                                editing,
                                name,
                                host,
                                port,
                                user,
                                group,
                                secret,
                                kind,
                                auth,
                                checking,
                                screen,
                            )
                        }
                    }
                }
            }
        }
    }
}

/// One saved machine: what it speaks, where it is, and when it was last used or why it cannot be
/// opened. Activating it asks to connect; its edit control opens the form over it.
view connection_row(
    c: Connection,
    asked: State<Text>,
    asking: State<Text>,
    editing: State<Option<Connection>>,
    name: State<Text>,
    host: State<Text>,
    port: State<Text>,
    user: State<Text>,
    group: State<Text>,
    secret: State<Text>,
    kind: State<ConnectionKind>,
    auth: State<AuthMethod>,
    checking: State<Bool>,
    screen: State<Text>,
) {
    ui.ListRow(
        c.name,
        item: c,
        icon: if c.kind == .ssh { .square_terminal } else { .screen_share },
        secondary: address(c),
        meta: row_meta(c),
        meta_tone: .warning,
        mark: if c.favorite { .pin },
        selected: c.id == asked,
        tone: if online(c) { .success } else { .neutral },
    ) {
        on activate: if profile_problem(c) != "" {
            set(editing, .some(c))
                .then(set(name, c.name))
                .then(set(host, c.host))
                .then(set(port, "{c.port}"))
                .then(set(user, c.user))
                .then(set(group, c.group))
                .then(set(secret, ""))
                .then(set(kind, c.kind))
                .then(set(auth, c.auth))
                .then(set(checking, true))
                .then(set(screen, "form"))
        } else {
            set(asking, "connect").then(set(asked, c.id))
        }
        trailing: {
            if profile_problem(c) == "" {
                if let at = c.last_used {
                    ui.Time(at, role: .caption, secondary: true, past: true)
                }
            }
        }
    }
    ui.ContextMenu {
        ui.MenuItem("Connect", icon: .plug) {
            on choose: set(asking, "connect").then(set(asked, c.id))
        }
        ui.MenuItem("Edit connection", icon: .pencil, key: "F2") {
            on choose: set(editing, .some(c))
                .then(set(name, c.name))
                .then(set(host, c.host))
                .then(set(port, "{c.port}"))
                .then(set(user, c.user))
                .then(set(group, c.group))
                .then(set(secret, ""))
                .then(set(kind, c.kind))
                .then(set(auth, c.auth))
                .then(set(checking, false))
                .then(set(screen, "form"))
        }
    }
}

/// The add and edit form. Save is the window bar's; problems show beside their fields once Save
/// was pressed, and follow each edit after.
view connection_form(
    r: Remote,
    editing: Option<Connection>,
    name: State<Text>,
    host: State<Text>,
    port: State<Text>,
    user: State<Text>,
    group: State<Text>,
    secret: State<Text>,
    kind: State<ConnectionKind>,
    auth: State<AuthMethod>,
    checking: Bool,
    screen: State<Text>,
) {
    let kinds = [ConnectionKind.ssh, ConnectionKind.vnc, ConnectionKind.rdp]
    let methods = methods_of(kind)
    scroll {
        page {
            ui.Section("Machine") {
                form {
                    field("Protocol") {
                        ui.ToggleGroup(selected: kinds.index_of(kind) ?? 0, label: "Protocol") {
                            ui.Segment("SSH")
                            ui.Segment("VNC")
                            ui.Segment("RDP")
                            on change(i: Int): set(kind, kinds.at(i) ?? .ssh)
                                .then(
                                    set(port, "{default_port(kinds.at(i) ?? .ssh)}"),
                                )
                                .then(
                                    set(auth, methods_of(kinds.at(i) ?? .ssh).first() ?? .password),
                                )
                        }
                    }
                    field("Name", error: if checking { name_problem(name) } else { "" }) {
                        ui.TextInput(bind: name, placeholder: "Home server", autofocus: true)
                    }
                    field("Host", error: if checking { host_problem(host) } else { "" }) {
                        ui.TextInput(bind: host, placeholder: "Host name or IP address")
                    }
                    field("Port", error: if checking { port_problem(port) } else { "" }) {
                        ui.TextInput(bind: port, placeholder: "Port")
                    }
                    if kind == .ssh {
                        field("User") {
                            ui.TextInput(bind: user, placeholder: "User name")
                        }
                    }
                    if methods.len() > 1 {
                        field("Sign in with") {
                            ui.ToggleGroup(
                                selected: methods.index_of(auth) ?? 0,
                                label: "Sign in with",
                            ) {
                                for m in methods {
                                    ui.Segment(auth_word(m))
                                }
                                on change(i: Int): set(auth, methods.at(i) ?? .password)
                            }
                        }
                    }
                    field(
                        if auth == .public_key { "Key passphrase" } else { "Password" },
                        help: "Kept in the workspace's vault.",
                    ) {
                        ui.TextInput(
                            bind: secret,
                            placeholder: if auth == .public_key { "Optional" } else { "" },
                            secret: true,
                        )
                    }
                    field("Group") {
                        ui.TextInput(bind: group, placeholder: "Optional")
                    }
                }
            }
            if let c = editing {
                ui.Section() {
                    ui.Button("Delete connection", icon: .trash, role: .destructive) {
                        on press: {
                            delete_connection(r, c)
                            set(screen, "list")
                        }
                    }
                }
            }
        }
    }
}

/// An open session: the terminal or the screen once it is up, and what it says until then or
/// once it ended.
view session_view(
    c: Connection,
    s: Session,
    actual_size: Bool,
    full: State<Bool>,
    asking: State<Text>,
    asked: State<Text>,
) {
    let word = said(s)
    if word.phase == "connected" and s.kind == .ssh and s.terminal != "" {
        ui.Terminal(s.terminal, label: "Remote terminal, {c.name}")
    } else if word.phase == "connected" and s.kind != .ssh {
        ui.Fullscreen(showing: full, name: "Remote desktop, {c.name}") {
            on exit: set(full, false)
            ui.Image(
                "tilo-live://{s.job}",
                alt: "Remote desktop, {c.name}",
                fit: if actual_size { .actual } else { .contain },
                input: s.job,
            )
        }
    } else if word.phase == "connecting" {
        ui.EmptyState(
            "Connecting to {c.name}",
            icon: if c.kind == .ssh { .square_terminal } else { .screen_share },
            message: "Remote is reaching the machine and signing in. It stops on its own after 45 seconds.",
            action: "Stop",
        ) {
            on act: close_session(s)
        }
    } else if word.phase == "closed" {
        ui.EmptyState(
            "Session ended",
            icon: if c.kind == .ssh { .square_terminal } else { .screen_share },
            message: "{c.name} closed the session.",
            action: "Reconnect",
        ) {
            on act: connect(c)
        }
    } else if word.phase == "need-trust" {
        ui.EmptyState(
            "Couldn't connect to {c.name}",
            icon: .shield_alert,
            message: word.why,
            error: true,
            action: "Trust key",
        ) {
            on act: set(asking, "trust").then(set(asked, c.id))
        }
    } else if word.phase == "key-changed" {
        ui.EmptyState(
            "Couldn't connect to {c.name}",
            icon: .shield_alert,
            message: word.why,
            error: true,
            action: "Forget key",
        ) {
            on act: forget_host_key(c)
        }
    } else if unreachable(c) != "" {
        ui.EmptyState(
            if c.kind == .rdp {
                "Remote can't open RDP connections"
            } else { "Couldn't connect to {c.name}" },
            icon: if c.kind == .ssh { .square_terminal } else { .screen_share },
            message: unreachable(c),
            error: true,
            action: "Close",
        ) {
            on act: close_session(s)
        }
    } else {
        ui.EmptyState(
            "Couldn't connect to {c.name}",
            icon: if c.kind == .ssh { .square_terminal } else { .screen_share },
            message: word.why,
            error: true,
            action: "Try again",
        ) {
            on act: connect(c)
        }
    }
    // What this device saw of the machine is noted as the session moves.
    if word.phase == "connected" or word.phase == "failed" {
        after 0s: observe(s)
    }
}

/// The question the window asks before it reaches a machine, ends a session or trusts a key. It
/// names what it is about as the connection stands now, so a member's change while it was asked
/// is what the person answers.
view confirmation(
    r: Remote,
    asking: State<Text>,
    asked: State<Text>,
    place: State<Text>,
    open: State<Text>,
) {
    if let c = connection_of(r, asked) {
        if asking == "connect" {
            ui.ModalDialog(
                title: "Connect to {c.name}?",
                open: true,
                primary: "Connect",
                secondary: "Cancel",
            ) {
                body: {
                    text(
                        "This opens a live session to {address(c)}. Remote checks the saved address and access again before anything is sent.",
                    )
                }
                on act_primary: {
                    let _ = connect(c)
                    set(asking, "").then(set(open, c.id)).then(set(place, "session"))
                }
                on dismiss: set(asking, "")
            }
        } else if asking == "disconnect" {
            ui.ModalDialog(
                title: "Close the session to {c.name}?",
                open: true,
                primary: "Close session",
                secondary: "Cancel",
            ) {
                body: {
                    text(
                        "The live session ends and its scrollback goes with it. The saved connection stays.",
                    )
                }
                on act_primary: {
                    disconnect(c)
                    set(asking, "").then(set(place, "all"))
                }
                on dismiss: set(asking, "")
            }
        } else if asking == "trust" {
            let key = shown_key(c)
            ui.ModalDialog(
                title: "Trust the SSH key of {c.name}?",
                open: true,
                primary: "Trust key",
                secondary: "Cancel",
            ) {
                body: {
                    stack(gap: .related) {
                        text(
                            "Check this key with the machine's owner before you trust it. Only this device remembers it.",
                        )
                        ui.Text("{key.algorithm} {key.fingerprint}", role: .code)
                    }
                }
                on act_primary: {
                    trust_host_key(c, key.fingerprint)
                    let _ = connect(c)
                    set(asking, "")
                }
                on dismiss: set(asking, "")
            }
        }
    }
}
