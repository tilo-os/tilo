app so.tilo.api-tester "API Tester" (
    icon: .globe,
    category: .development,
    tagline: "Sends HTTP requests and shows the responses",
    keywords: ["postman", "insomnia", "rest", "http", "requests", "endpoint", "graphql", "api", "client", "debug", "test"],
)

/// The methods a request is sent with, in the order the chooser lists them.
enum HttpMethod {
    get
    post
    put
    patch
    delete
    head
    options
}

/// How a request signs in.
enum AuthScheme {
    none
    bearer
    basic
    api_key
}

/// What a request's body is.
enum BodyMode {
    none
    json
    form
    raw
}

/// One key and value of a request: a query parameter, a header or a form field.
type Row (untitled: "New row") {
    key: Text
    value: Text
    /// Included: a row switched off stays listed and is not sent.
    enabled: Bool = true
}

/// What a request signs in with. The credential itself never enters the workspace: `reference`
/// names what this device keeps for it, and only that device sends it.
type Credentials {
    scheme: AuthScheme = .none
    /// The user name of basic authentication: public, as the name of the account is.
    username: Text
    /// The header an API key travels in.
    header: Text
    /// The device-local name of the credential.
    reference: Text
}

/// One request, saved in a collection or being built.
type Request (untitled: "New request") {
    name: Text
    method: HttpMethod = .get
    url: Text
    params: Collection<Row>
    headers: Collection<Row>
    auth: Credentials
    body_mode: BodyMode = .none
    /// The body as typed: JSON, raw text or a template.
    body: Text
    form: Collection<Row>
}

/// What a service answered.
type Answer (untitled: "Response") {
    status: Int
    status_text: Text
    duration_ms: Int
    size_bytes: Int
    /// Whether `size_bytes` is the body's size, rather than how much of an undeclared body
    /// arrived before the download stopped.
    size_exact: Bool
    /// How many of the body's bytes the view shows.
    shown_bytes: Int
    /// Whether the body is binary, so the view shows a hex dump of its start.
    binary: Bool
    content_type: Text
    /// The body: as received, or a hex dump when it is binary.
    body: Text
    /// The body laid out as JSON reads, for the pretty view; the body as received otherwise.
    pretty: Text
    headers: Collection<AnswerHeader>
}

/// One header a service answered with.
type AnswerHeader (untitled: "Header") {
    name: Text
    value: Text
}

/// A set of `{{variable}}` substitutions, resolved into a request just before it is sent.
type Environment (untitled: "Untitled environment") {
    name: Text
    variables: Collection<Variable>
}

/// One variable of an environment. A secret variable's value stays on each device: the workspace
/// keeps the reference and the engine sends the value.
type Variable (untitled: "variable") {
    key: Text
    value: Text
    enabled: Bool = true
    /// Whether the value is a credential kept on each device.
    secret: Bool = false
    /// The device-local name of a secret value.
    reference: Text
}

/// One request that was sent, as history keeps it.
type Sent (untitled: "Sent request") {
    /// The method and the address without credentials, query values or fragment, so it can name
    /// the entry anywhere.
    route: Text
    method: HttpMethod
    url: Text
    status: Int
    duration_ms: Int
    at: Time
    /// The request as it was authored when it was sent, with variables as typed and never a
    /// credential, so reopening the entry brings the whole request back.
    replay: Text
}

/// What a saved collection of requests is called.
type SavedCollection (untitled: "Untitled collection") {
    name: Text
    requests: Collection<Request>
}

/// The workspace's API tester: collections of requests, the environments that fill their
/// variables, the request being built, and what was sent.
type Workspace {
    collections: Collection<SavedCollection>
    environments: Collection<Environment>
    /// Whose variables resolve into what is sent.
    active_environment: Option<Environment> = .none
    /// The workspace's request builder: the request being typed, overwritten when another is
    /// opened.
    builder: Request
    /// The saved request the builder holds a copy of.
    opened: Option<Request> = .none
    /// What was sent, newest first.
    history: Collection<Sent>
    /// A request is on its way; `job` names it on this device.
    sending: Bool = false
    job: Text = ""
    /// The request as authored when it went out, kept for the history entry it makes.
    sent_replay: Text = ""
    /// What the service answered, once it did.
    answer: Answer
    /// Whether the response pane shows an answer or a failure at all.
    answered: Bool = false
    /// Why the last send failed, in the device's words.
    failure: Text = ""
    /// One line under the response about the last copy.
    notice: Text = ""
}

root tester: Workspace

default view tester for Workspace

/// Starts a blank request in the builder, leaving the one it held.
// ---------------------------------------------------------------------------
// What a person can do
// ---------------------------------------------------------------------------
primary action new_request(w: Workspace) -> Request (
    icon: .plus,
    label: "New request",
    did: "started a request",
) {
    w.builder.name = ""
    w.builder.method = .get
    w.builder.url = ""
    w.builder.body_mode = .none
    w.builder.body = ""
    w.builder.auth.scheme = .none
    w.builder.auth.username = ""
    w.builder.auth.header = ""
    w.builder.auth.reference = ""
    for entry in w.builder.params {
        w.builder.params.remove(entry)
    }
    for entry in w.builder.headers {
        w.builder.headers.remove(entry)
    }
    for entry in w.builder.form {
        w.builder.form.remove(entry)
    }
    w.opened = .none
    w.answered = false
    w.failure = ""
    w.notice = ""
    w.builder
}

/// Creates a collection to keep requests in.
action new_collection(w: Workspace, name: Text) -> SavedCollection (
    icon: .plus,
    label: "New collection",
) {
    require name.trim() != "" "A collection needs a name"
    w.collections.insert(SavedCollection(name: name.trim()))
}

/// Renames a collection.
action rename_collection(c: SavedCollection, name: Text) (
    icon: .pencil,
    label: "Rename collection",
) {
    require name.trim() != "" "A collection needs a name"
    c.name = name.trim()
}

/// Deletes a collection and the requests saved in it.
destructive action delete_collection(w: Workspace, c: SavedCollection) (
    icon: .trash,
    label: "Delete collection",
) {
    require w.collections.contains(c) "That collection is gone"
    for r in c.requests {
        if w.opened == .some(r) {
            w.opened = .none
        }
    }
    w.collections.remove(c)
}

/// Adds a request to a collection.
action add_request(c: SavedCollection, name: Text = "") -> Request (
    icon: .plus,
    label: "Add request",
) {
    c.requests.insert(Request(name: name.trim()))
}

/// Saves the request in the builder into a collection.
action save_request(w: Workspace, c: SavedCollection) -> Request (
    icon: .check,
    label: "Save request",
    did: "saved a request",
) {
    if let o = w.opened {
        if c.requests.contains(o) {
            fill_from(o, w.builder)
            return o
        }
    }
    let saved = c.requests.insert(Request(name: w.builder.name))
    fill_from(saved, w.builder)
    w.opened = .some(saved)
    saved
}

/// Copies everything a request holds into another one.
action fill_from(dest: Request, src: Request) {
    dest.name = src.name
    dest.method = src.method
    dest.url = src.url
    dest.body_mode = src.body_mode
    dest.body = src.body
    dest.auth.scheme = src.auth.scheme
    dest.auth.username = src.auth.username
    dest.auth.header = src.auth.header
    dest.auth.reference = src.auth.reference
    for r in dest.params {
        dest.params.remove(r)
    }
    for r in src.params {
        dest.params.insert(Row(key: r.key, value: r.value, enabled: r.enabled))
    }
    for r in dest.headers {
        dest.headers.remove(r)
    }
    for r in src.headers {
        dest.headers.insert(Row(key: r.key, value: r.value, enabled: r.enabled))
    }
    for r in dest.form {
        dest.form.remove(r)
    }
    for r in src.form {
        dest.form.insert(Row(key: r.key, value: r.value, enabled: r.enabled))
    }
}

/// Opens a saved request in the builder.
action open_request(w: Workspace, r: Request) (icon: .pencil, did: "opened a request") {
    fill_from(w.builder, r)
    w.opened = .some(r)
    w.answered = false
    w.failure = ""
}

/// Renames the request in the builder.
action rename_draft(w: Workspace, name: Text) (icon: .pencil, label: "Rename request") {
    require name.trim() != "" "A request needs a name"
    w.builder.name = name.trim()
}

/// Deletes a saved request.
destructive action delete_request(w: Workspace, r: Request) (
    icon: .trash,
    label: "Delete request",
) {
    if w.opened == .some(r) {
        w.opened = .none
    }
    if let holder = w.collections.find(c -> c.requests.contains(r)) { holder.requests.remove(r) }
}

/// Deletes the saved request the builder holds a copy of.
destructive action delete_opened_request(w: Workspace) (
    icon: .trash,
    label: "Delete the saved request",
) {
    if let o = w.opened { delete_request(w, o) }
}

/// Adds a query parameter to the request in the builder.
action add_param(r: Request) (icon: .plus, label: "Add parameter") {
    r.params.insert(Row(key: "", value: ""))
}

/// Takes a query parameter off the request in the builder.
destructive action delete_param(r: Request, entry: Row) (icon: .trash, label: "Delete parameter") {
    r.params.remove(entry)
}

/// Adds a header to the request in the builder.
action add_header(r: Request) (icon: .plus, label: "Add header") {
    r.headers.insert(Row(key: "", value: ""))
}

/// Takes a header off the request in the builder.
destructive action delete_header(r: Request, entry: Row) (icon: .trash, label: "Delete header") {
    r.headers.remove(entry)
}

/// Adds a form field to the request in the builder.
action add_form_field(r: Request) (icon: .plus, label: "Add form field") {
    r.form.insert(Row(key: "", value: ""))
}

/// Takes a form field off the request in the builder.
destructive action delete_form_field(r: Request, entry: Row) (
    icon: .trash,
    label: "Delete form field",
) {
    r.form.remove(entry)
}

/// Creates an environment and starts using it.
action new_environment(w: Workspace, name: Text) -> Environment (
    icon: .plus,
    label: "New environment",
) {
    require name.trim() != "" "An environment needs a name"
    let created = w.environments.insert(Environment(name: name.trim()))
    w.active_environment = .some(created)
    created
}

/// Renames an environment.
action rename_environment(e: Environment, name: Text) (icon: .pencil, label: "Rename environment") {
    require name.trim() != "" "An environment needs a name"
    e.name = name.trim()
}

/// Deletes an environment and the variables it held.
destructive action delete_environment(w: Workspace, e: Environment) (
    icon: .trash,
    label: "Delete environment",
) {
    for v in e.variables {
        forget_value(v.reference)
    }
    if w.active_environment == .some(e) {
        w.active_environment = .none
    }
    w.environments.remove(e)
}

/// Deletes the environment in use.
destructive action delete_active_environment(w: Workspace) (
    icon: .trash,
    label: "Delete the environment",
) {
    if let e = w.active_environment { delete_environment(w, e) }
}

/// Switches the environment whose variables fill what is sent.
action use_environment(w: Workspace, e: Environment) (
    icon: .check,
    label: "Use environment",
    did: "switched environment",
) {
    require w.environments.contains(e) "That environment is gone"
    w.active_environment = .some(e)
}

/// Sends with no environment: every `{{variable}}` stays as typed.
action no_environment(w: Workspace) (
    icon: .x,
    label: "No environment",
    did: "stopped using an environment",
) {
    w.active_environment = .none
}

/// Adds a variable to an environment. A secret's value is typed after it stands.
action add_variable(e: Environment, key: Text, secret: Bool = false) -> Variable (
    icon: .plus,
    label: "Add variable",
) {
    require key.trim() != "" "A variable needs a key"
    e.variables.insert(Variable(key: key.trim(), secret: secret))
}

/// Takes a variable off its environment, and its value off this device.
destructive action delete_variable(e: Environment, v: Variable) (
    icon: .trash,
    label: "Delete variable",
) {
    forget_value(v.reference)
    e.variables.remove(v)
}

/// Keeps the value typed for the request's sign-in where the workspace cannot read it, and
/// remembers only the reference.
action keep_credential(w: Workspace, value: Text) (
    icon: .key,
    label: "Store sign-in",
    did: "stored a sign-in",
) {
    require value != "" "Type the credential first"
    match engine.call("keep_credential", json.encode(KeepAsk(value: value))) {
        .err(why) => {
            w.failure = why
            w.answered = true
        }
        .ok(text) => match json.decode<KeptCredential>(text) {
            .err(why) => {
                w.failure = why
                w.answered = true
            }
            .ok(kept) => {
                forget_value(w.builder.auth.reference)
                w.builder.auth.reference = kept.reference
                w.failure = ""
                w.notice = "The sign-in is kept on this device"
            }
        },
    }
}

/// Keeps the value typed for a secret variable where the workspace cannot read it.
action keep_variable(v: Variable, value: Text) (
    icon: .key,
    label: "Store value",
    did: "stored a value",
) {
    require v.secret "This variable's value is shared; edit it in its field"
    require value != "" "Type the value first"
    match engine.call("keep_credential", json.encode(KeepAsk(value: value))) {
        .err(why) => {
            v.value = ""
        }
        .ok(text) => match json.decode<KeptCredential>(text) {
            .err(why) => {
                v.value = ""
            }
            .ok(kept) => {
                forget_value(v.reference)
                v.reference = kept.reference
            }
        },
    }
}

/// Takes a stored value off this device.
action forget_value(reference: Text) (icon: .trash, label: "Forget value") {
    if reference != "" {
        engine.call("forget_credential", json.encode(ForgetAsk(reference: reference)))
    }
}

/// Changes the scheme, taking the credential of the one before off this device.
action set_scheme(w: Workspace, scheme: AuthScheme) {
    if w.builder.auth.scheme != scheme {
        forget_value(w.builder.auth.reference)
        w.builder.auth.reference = ""
    }
    w.builder.auth.scheme = scheme
}

/// Takes the request's sign-in off this device.
action forget_credential(w: Workspace) {
    forget_value(w.builder.auth.reference)
    w.builder.auth.reference = ""
}

/// Sends what the builder holds to the address it names, in the environment in use.
action send(w: Workspace) uses time (icon: .send, label: "Send", did: "sent a request") {
    require not w.sending "A request is already on its way"
    require w.builder.url.trim() != "" "Enter a URL to send the request to"
    w.failure = ""
    w.notice = ""
    w.answered = false
    let ask = ask_of(w)
    let replay = json.encode(replay_of(w))
    match engine.start("send", json.encode(ask)) {
        .err(why) => {
            w.failure = why
            w.answered = true
        }
        .ok(job) => {
            w.job = job
            w.sending = true
            w.sent_replay = replay
        }
    }
}

/// Stops waiting for the service to answer.
action cancel_send(w: Workspace) (icon: .x, label: "Cancel request") {
    if w.sending and w.job != "" { engine.cancel(w.job) }
    w.sending = false
    w.job = ""
    w.failure = "The request was cancelled."
    w.answered = true
}

/// Asks the device how the request on its way is doing, and keeps what it answered.
action poll_send(w: Workspace) uses time {
    if not w.sending or w.job == "" {
        return
    }
    match engine.status(w.job) {
        .err(why) => {
            w.failure = why
            w.sending = false
            w.job = ""
            w.answered = true
        }
        .ok(text) => match json.decode<SendState>(text) {
            .err(why) => {
                w.failure = why
                w.sending = false
                w.job = ""
                w.answered = true
            }
            .ok(state) => {
                if state.state == "done" {
                    w.sending = false
                    w.job = ""
                    w.answered = true
                    w.failure = ""
                    keep_answer(w, state.answer)
                    record_sent(w, state.answer)
                    w.sent_replay = ""
                } else if state.state == "failed" {
                    w.sending = false
                    w.job = ""
                    w.answered = true
                    w.failure = if state.error == "" { "The request failed" } else { state.error }
                } else if state.state == "cancelled" {
                    w.sending = false
                    w.job = ""
                    w.answered = true
                    w.failure = "The request was cancelled."
                }
            }
        },
    }
}

/// A request on its way is followed once a second, so its answer reaches the window without
/// anything polling by hand.
trigger watch_send(w: Workspace) every 1s: poll_send(w)

/// Opens a request that was sent before, with what it held when it went out.
action open_sent(w: Workspace, s: Sent) (icon: .history, did: "opened a sent request") {
    match json.decode<Replay>(s.replay) {
        .err(why) => {
            w.failure = why
            w.answered = true
        }
        .ok(replay) => {
            w.builder.name = replay.name
            w.builder.method = method_of(replay.method)
            w.builder.url = replay.url
            w.builder.body_mode = body_of(replay.body_mode)
            w.builder.body = replay.body
            w.builder.auth.scheme = scheme_of(replay.scheme)
            w.builder.auth.username = replay.username
            w.builder.auth.header = replay.header
            w.builder.auth.reference = replay.reference
            for entry in w.builder.params {
                w.builder.params.remove(entry)
            }
            for pair in replay.params {
                w.builder.params.insert(row_of(pair))
            }
            for entry in w.builder.headers {
                w.builder.headers.remove(entry)
            }
            for pair in replay.headers {
                w.builder.headers.insert(row_of(pair))
            }
            for entry in w.builder.form {
                w.builder.form.remove(entry)
            }
            for pair in replay.form {
                w.builder.form.insert(row_of(pair))
            }
            w.opened = .none
            w.answered = false
            w.failure = ""
        }
    }
}

/// The row a pair of the request as authored names.
fn row_of(pair: List<Text>) -> Row {
    Row(key: pair.at(0) ?? "", value: pair.at(1) ?? "", enabled: pair.at(2) != "off")
}

/// Saves a sent request as a request in a collection, named by its route unless it had a name.
action save_sent(w: Workspace, s: Sent, c: SavedCollection) -> Request (
    icon: .check,
    label: "Save sent request",
) {
    match json.decode<Replay>(s.replay) {
        .err(why) => {
            w.failure = why
            w.answered = true
            c.requests.insert(Request(name: "New request"))
        }
        .ok(replay) => {
            let name = if replay.name != "" {
                replay.name
            } else {
                "{method_text(s.method)} {short_path(s.url)}"
            }
            let saved = c.requests.insert(Request(name: name))
            saved.method = method_of(replay.method)
            saved.url = replay.url
            saved.body_mode = body_of(replay.body_mode)
            saved.body = replay.body
            saved.auth.scheme = scheme_of(replay.scheme)
            saved.auth.username = replay.username
            saved.auth.header = replay.header
            for pair in replay.params {
                saved.params.insert(row_of(pair))
            }
            for pair in replay.headers {
                saved.headers.insert(row_of(pair))
            }
            for pair in replay.form {
                saved.form.insert(row_of(pair))
            }
            saved
        }
    }
}

/// Forgets a sent request.
destructive action delete_sent(w: Workspace, s: Sent) (icon: .trash, label: "Forget sent request") {
    w.history.remove(s)
}

/// Copies the response body, exactly as the response pane shows it.
action copy_body(w: Workspace) uses clipboard (icon: .copy, label: "Copy body") {
    require w.answered "There is no response to copy yet"
    clipboard.copy(w.answer.body)
    w.notice = "The body is copied"
}

/// Copies every response header, one `Name: value` per line.
action copy_headers(w: Workspace) uses clipboard (icon: .list, label: "Copy headers") {
    require w.answered "There is no response to copy yet"
    clipboard.copy(header_text(w.answer))
    w.notice = "The headers are copied"
}

/// Copies the request as authored, with its `{{variables}}` and never a credential.
action copy_request(w: Workspace) uses clipboard (icon: .clipboard_paste, label: "Copy request") {
    clipboard.copy(request_text(w.builder))
    w.notice = "The request is copied"
}

/// Dismisses the line under the response.
action dismiss_notice(w: Workspace) {
    w.notice = ""
}

/// What the app asks its engine to send: the request as typed, its variables, and the reference
/// that names its sign-in on this device. It is flat because a written record holds values only.
// ---------------------------------------------------------------------------
// What the app asks its engine, and what the engine answers
// ---------------------------------------------------------------------------
type SendAsk {
    method: Text
    url: Text
    params: List<List<Text>>
    headers: List<List<Text>>
    /// Each variable as name, value and reference, a reference only for a secret.
    vars: List<List<Text>>
    auth_scheme: Text
    auth_username: Text
    auth_header: Text
    auth_reference: Text
    body_mode: Text
    body: Text
    form: List<List<Text>>
}

/// What the app asks its device to keep.
type KeepAsk {
    value: Text
}

/// What is taken away again.
type ForgetAsk {
    reference: Text
}

/// The reference a kept value answers with.
type KeptCredential {
    reference: Text = ""
}

/// How far a request on its way has got: its state, and what it answered as one JSON text.
type SendState {
    state: Text = ""
    /// The answer as JSON text, empty while the request is on its way.
    answer: Text = ""
    error: Text = ""
}

/// What a service answered, as the engine writes it.
type Answered {
    status: Int = 0
    status_text: Text = ""
    duration_ms: Int = 0
    size_bytes: Int = 0
    size_exact: Bool = false
    shown_bytes: Int = 0
    binary: Bool = false
    content_type: Text = ""
    body: Text = ""
    headers: List<List<Text>>
}

/// A request as it was authored, for a history entry to bring back.
type Replay {
    name: Text = ""
    method: Text = "GET"
    url: Text = ""
    params: List<List<Text>>
    headers: List<List<Text>>
    form: List<List<Text>>
    scheme: Text = "none"
    username: Text = ""
    header: Text = ""
    reference: Text = ""
    body_mode: Text = "none"
    body: Text = ""
}

/// The request the builder holds, as the engine reads it.
fn ask_of(w: Workspace) -> SendAsk {
    SendAsk(
        method: method_text(w.builder.method),
        url: w.builder.url,
        params: pairs_of(w.builder.params),
        headers: pairs_of(w.builder.headers),
        vars: variables_of(w),
        auth_scheme: scheme_text(w.builder.auth.scheme),
        auth_username: w.builder.auth.username,
        auth_header: w.builder.auth.header,
        auth_reference: w.builder.auth.reference,
        body_mode: body_text(w.builder.body_mode),
        body: w.builder.body,
        form: pairs_of(w.builder.form),
    )
}

/// The rows that are sent, key and value as written.
fn pairs_of(rows: List<Row>) -> List<List<Text>> {
    rows.filter(r -> r.enabled and r.key.trim() != "").map(r -> [r.key, r.value])
}

/// The enabled variables of the environment in use, each as name, value and reference.
fn variables_of(w: Workspace) -> List<List<Text>> {
    if let e = w.active_environment {
        return e.variables
            .filter(v -> v.enabled and v.key.trim() != "")
            .map(v -> [v.key.trim(), v.value, v.reference])
    }
    []
}

/// The request as it was authored, for a history entry.
fn replay_of(w: Workspace) -> Replay {
    Replay(
        name: w.builder.name,
        method: method_text(w.builder.method),
        url: w.builder.url,
        params: rows_of(w.builder.params),
        headers: rows_of(w.builder.headers),
        form: rows_of(w.builder.form),
        scheme: scheme_text(w.builder.auth.scheme),
        username: w.builder.auth.username,
        header: w.builder.auth.header,
        reference: w.builder.auth.reference,
        body_mode: body_text(w.builder.body_mode),
        body: w.builder.body,
    )
}

/// Every row, switched-off ones marked so they come back switched off.
fn rows_of(rows: List<Row>) -> List<List<Text>> {
    rows.map(r -> [r.key, r.value, if r.enabled { "on" } else { "off" }])
}

/// Keeps the answer in the workspace, so the window and an agent read the same one.
// ---------------------------------------------------------------------------
// Keeping what was answered
// ---------------------------------------------------------------------------
action keep_answer(w: Workspace, answer: Text) {
    match json.decode<Answered>(answer) {
        .err(why) => {
            w.failure = why
        }
        .ok(a) => {
            for old in w.answer.headers {
                w.answer.headers.remove(old)
            }
            w.answer.status = a.status
            w.answer.status_text = a.status_text
            w.answer.duration_ms = a.duration_ms
            w.answer.size_bytes = a.size_bytes
            w.answer.size_exact = a.size_exact
            w.answer.shown_bytes = a.shown_bytes
            w.answer.binary = a.binary
            w.answer.content_type = a.content_type
            w.answer.body = a.body
            w.answer.pretty = if reads_as_json(a.content_type, a.body) {
                pretty_json(a.body)
            } else { a.body }
            for pair in a.headers {
                w.answer.headers.insert(
                    AnswerHeader(name: pair.at(0) ?? "", value: pair.at(1) ?? ""),
                )
            }
        }
    }
}

/// Adds the request that just came back to the workspace's history, newest first.
action record_sent(w: Workspace, answer: Text) uses time {
    match json.decode<Answered>(answer) {
        .err(why) => {
            w.failure = why
        }
        .ok(a) => {
            let method = method_of(method_text(w.builder.method))
            w.history.insert(
                Sent(
                    route: route_of(method, w.builder.url, a.status),
                    method: method,
                    url: w.builder.url,
                    status: a.status,
                    duration_ms: a.duration_ms,
                    at: now,
                    replay: w.sent_replay,
                ),
            )
            w.sent_replay = ""
            // The workspace keeps the last fifty sends; the oldest goes as a new one arrives.
            if w.history.len() > 50 {
                if let oldest = w.history.sort_by(s -> s.at).first() { w.history.remove(oldest) }
            }
        }
    }
}

/// The word a method is sent as.
// ---------------------------------------------------------------------------
// Words and small readings
// ---------------------------------------------------------------------------
fn method_text(m: HttpMethod) -> Text {
    match m {
        .get => "GET",
        .post => "POST",
        .put => "PUT",
        .patch => "PATCH",
        .delete => "DELETE",
        .head => "HEAD",
        .options => "OPTIONS",
    }
}

/// The method a word names; an unknown word reads as GET.
fn method_of(text: Text) -> HttpMethod {
    let word = text.upper()
    if word == "POST" {
        return .post
    }
    if word == "PUT" {
        return .put
    }
    if word == "PATCH" {
        return .patch
    }
    if word == "DELETE" {
        return .delete
    }
    if word == "HEAD" {
        return .head
    }
    if word == "OPTIONS" {
        return .options
    }
    .get
}

/// The word a scheme is kept as.
fn scheme_text(s: AuthScheme) -> Text {
    match s {
        .none => "none",
        .bearer => "bearer",
        .basic => "basic",
        .api_key => "api_key",
    }
}

fn scheme_of(text: Text) -> AuthScheme {
    if text == "bearer" {
        return .bearer
    }
    if text == "basic" {
        return .basic
    }
    if text == "api_key" {
        return .api_key
    }
    .none
}

/// The word a body mode is kept as.
fn body_text(m: BodyMode) -> Text {
    match m {
        .none => "none",
        .json => "json",
        .form => "form",
        .raw => "raw",
    }
}

fn body_of(text: Text) -> BodyMode {
    if text == "json" {
        return .json
    }
    if text == "form" {
        return .form
    }
    if text == "raw" {
        return .raw
    }
    .none
}

/// The path an address names, for a history row's short label.
fn short_path(url: Text) -> Text {
    let no_scheme = url.split("://").at(1) ?? url
    return match no_scheme.find("/") {
        .some(at) => no_scheme.slice(at, no_scheme.len()),
        .none => no_scheme,
    }
}

/// Enough of an address to find it again, without credentials, query values or a fragment. An
/// address that does not read gets a truthful label rather than its text.
fn route_of(m: HttpMethod, url: Text, status: Int) -> Text {
    let method = method_text(m)
    let parts = url.split("://")
    if parts.len() < 2 {
        return "{method} request returned status {status}"
    }
    let scheme = parts.at(0) ?? ""
    let authority = (parts.at(1) ?? "").split("?").at(0) ?? ""
    let safe = authority.split("#").at(0) ?? ""
    return match safe.find("/") {
        .some(at) => {
            let host = last_after_at(safe.slice(0, at))
            let rest = safe.slice(at + 1, safe.len())
            "{method} {scheme}://{host}/{rest} returned status {status}"
        }
        .none => {
            let host = last_after_at(safe)
            "{method} {scheme}://{host} returned status {status}"
        }
    }
}

/// What names a host once the user information before the last `@` is taken off.
fn last_after_at(text: Text) -> Text {
    text.split("@").last() ?? ""
}

/// The variables the text refers to, in the order they first appear.
fn referenced_vars(text: Text) -> List<Text> {
    referenced_in(text, [])
}

fn referenced_in(text: Text, found: List<Text>) -> List<Text> {
    let open = text.find("\{\{")
    if let at = open {
        let rest = text.slice(at + 2, text.len())
        if let close = rest.find("\}\}") {
            let name = rest.slice(0, close).trim()
            let more = referenced_in(rest.slice(close + 2, rest.len()), found)
            if name == "" or more.contains(name) {
                return more
            }
            return more + [name]
        }
    }
    found
}

/// What the address's `{{variables}}` resolve to, as one quiet line: which resolve, and which
/// this environment has no value for. Empty when the address has none.
fn variable_hint(w: Workspace) -> Text {
    let refs = referenced_vars(w.builder.url)
    if refs.is_empty() {
        return ""
    }
    let unresolved = refs.filter(name -> not resolved(w, name))
    if unresolved.is_empty() {
        if refs.len() == 1 {
            return "1 variable resolved"
        }
        return "{refs.len()} variables resolved"
    }
    if unresolved.len() == 1 {
        return "Unresolved variable: {unresolved.join(", ")}"
    }
    "Unresolved variables: {unresolved.join(", ")}"
}

/// Whether the environment in use holds a value for `name`.
fn resolved(w: Workspace, name: Text) -> Bool {
    if let e = w.active_environment {
        return e.variables.find(v -> v.enabled and v.key.trim() == name) != .none
    }
    false
}

/// A duration as a person reads it: `84 ms`, `1.20 s`.
fn duration_text(ms: Int) -> Text {
    if ms < 1000 {
        return "{ms} ms"
    }
    let tenths = ms / 10
    "{tenths / 100}.{two_digits(tenths % 100)} s"
}

/// A part of a hundred as two digits, with the leading zero it needs.
fn two_digits(n: Int) -> Text {
    if n < 10 {
        return "0{n}"
    }
    "{n}"
}

/// How many bytes read to a person: `812 B`, `4.2 KB`, `2.0 MB`.
fn size_text(bytes: Int) -> Text {
    if bytes < 1000 {
        return "{bytes} B"
    }
    if bytes < 1000000 {
        let tenths = bytes / 100
        return "{tenths / 10}.{tenths % 10} KB"
    }
    if bytes < 1000000000 {
        let tenths = bytes / 100000
        return "{tenths / 10}.{tenths % 10} MB"
    }
    let tenths = bytes / 100000000
    "{tenths / 10}.{tenths % 10} GB"
}

/// The body's size; a lower bound when the service declared none and only its start arrived.
fn size_caption(a: Answer) -> Text {
    if a.size_exact {
        return size_text(a.size_bytes)
    }
    "More than {size_text(a.size_bytes)}"
}

/// How the response pane shows the body when it is not the whole of it, and nothing when it is.
fn shown_caption(a: Answer) -> Text {
    let partial = not a.size_exact or a.shown_bytes < a.size_bytes
    if a.binary and partial {
        return "First {size_text(a.shown_bytes)} shown as hex"
    }
    if a.binary {
        return "Shown as hex"
    }
    if partial {
        return "First {size_text(a.shown_bytes)} shown"
    }
    ""
}

/// Whether a body reads as JSON, so the pane colours it as such.
fn reads_as_json(content_type: Text, body: Text) -> Bool {
    let head = body.trim()
    content_type.contains("json") or head.starts_with("\{") or head.starts_with("[")
}

/// Every response header, one `Name: value` per line.
fn header_text(a: Answer) -> Text {
    a.headers.map(h -> "{h.name}: {h.value}").join("\n")
}

/// The request as authored, with its `{{variables}}` and never a credential.
fn request_text(r: Request) -> Text {
    let head = "{method_text(r.method)} {r.url}"
    let headers = r.headers.filter(h -> h.enabled).map(h -> "{h.key}: {h.value}").join("\n")
    let body = if r.body.trim() == "" { "" } else { "\n\n{r.body}" }
    if headers == "" {
        return "{head}\n{body}"
    }
    "{head}\n{headers}\n{body}"
}

/// The tester's window: the collections and history in the sidebar, the request being built over
/// what the service answered, and the sheets that collect a name or a value.
// ---------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------
view tester(w: Workspace) {
    state tab: Int = 0
    state response_view: Int = 0
    state selected: Text = ""
    state sheet: Text = ""
    state typed_name: Text = ""
    state typed_key: Text = ""
    state typed_secret: Text = ""
    state form_secret: Bool = false
    state target_collection: Option<SavedCollection> = .none
    state target_variable: Option<Variable> = .none
    state target_sent: Option<Sent> = .none
    ui.NavShell(content_kind: .document, title: draft_title(w), sidebar_title: "API Tester") {
        sidebar_actions: {
            ui.Button("New collection", icon: .plus, icon_only: true) {
                on press: set(typed_name, "").then(set(sheet, "collection"))
            }
            environment_menu(
                w,
                sheet,
                typed_name,
                typed_key,
                form_secret,
                target_variable,
                target_collection,
            )
        }
        sidebar: {
            ui.NavList(selected: selected) {
                if w.collections.is_empty() {
                    ui.NavSection("Collections") {
                        ui.NavItem("no-collections", "No collections yet", icon: .folder) {
                            on select: set(selected, "")
                        }
                    }
                }
                for c in w.collections.sort_by(x -> x.name) {
                    ui.NavSection(c.name, collapsible: true, add: add_request(c)) {
                        for r in c.requests.sort_by(x -> x.name) {
                            ui.NavItem(
                                "request:{r.id()}",
                                "{method_text(r.method)} {named(r)}",
                                icon: if r.method == .get {
                                    .arrow_down
                                } else if r.method == .post { .arrow_up } else { .pencil },
                                item: r,
                            ) {
                                on select: open_request(w, r)
                                    .then(
                                        set(selected, "request:{r.id()}"),
                                    )
                            }
                        }
                    }
                }
                if not w.history.is_empty() {
                    ui.NavSection("History") {
                        for s in w.history.sort_by(x -> x.at, desc) {
                            ui.NavItem(
                                "sent:{s.id()}",
                                "{method_text(s.method)} {short_path(s.url)}",
                                icon: if s.status >= 500 {
                                    .circle_x
                                } else if s.status >= 400 {
                                    .triangle_alert
                                } else { .circle_check },
                                count: s.status,
                                item: s,
                            ) {
                                on select: open_sent(w, s)
                                    .then(
                                        set(selected, "sent:{s.id()}"),
                                    )
                                    .then(
                                        set(target_sent, .some(s)),
                                    )
                            }
                        }
                    }
                }
            }
        }
        actions: {
            if w.sending {
                ui.Button("Cancel request", icon: .x) {
                    on press: cancel_send(w)
                }
            } else {
                ui.Button("Send", icon: .send, role: .primary) {
                    on press: send(w)
                }
            }
            save_menu(w, sheet, target_sent)
            request_menu(w, sheet, typed_name, target_sent)
        }
        content: {
            stack(gap: .tight) {
                url_bar(w)
                let hint = variable_hint(w)
                if hint != "" {
                    ui.Text(hint, role: .caption, secondary: true)
                }
                request_panel(w, tab)
                response_pane(w, response_view)
            }
        }
    }
    sheets(
        w,
        sheet,
        typed_name,
        typed_key,
        typed_secret,
        form_secret,
        target_collection,
        target_variable,
    )
}

/// The window's title: what the request is called, or the address it names.
fn draft_title(w: Workspace) -> Text {
    if w.builder.name != "" {
        return w.builder.name
    }
    if w.builder.url != "" {
        return short_path(w.builder.url)
    }
    "New request"
}

/// A request's name, with the one it starts under when it has none.
fn named(r: Request) -> Text {
    if r.name != "" {
        return r.name
    }
    "New request"
}

/// The environment picker: whose variables fill what is sent, and the commands that manage
/// environments, their variables and the collections.
view environment_menu(
    w: Workspace,
    sheet: State<Text>,
    typed_name: State<Text>,
    typed_key: State<Text>,
    form_secret: State<Bool>,
    target_variable: State<Option<Variable>>,
    target_collection: State<Option<SavedCollection>>,
) {
    ui.MenuButton(
        if let e = w.active_environment { e.name } else { "No environment" },
        icon: .sliders_horizontal,
    ) {
        for e in w.environments.sort_by(x -> x.name) {
            ui.MenuItem(e.name, checked: w.active_environment == .some(e)) {
                on choose: use_environment(w, e)
            }
        }
        if w.active_environment != .none {
            ui.MenuItem("No environment") {
                on choose: no_environment(w)
            }
        }
        ui.MenuSeparator()
        ui.MenuItem("New environment…", icon: .plus) {
            on choose: set(typed_name, "").then(set(sheet, "environment"))
        }
        if let e = w.active_environment {
            ui.MenuItem("Rename environment…", icon: .pencil) {
                on choose: set(typed_name, e.name).then(set(sheet, "rename-environment"))
            }
            ui.MenuItem("Variables…", icon: .list) {
                on choose: set(sheet, "variables")
            }
            ui.MenuItem("Add variable…", icon: .plus) {
                on choose: set(typed_key, "")
                    .then(set(form_secret, false))
                    .then(set(sheet, "variable"))
            }
            for v in e.variables.sort_by(x -> x.key) {
                ui.MenuItem("Delete the variable {v.key}", icon: .trash, destructive: true) {
                    on choose: delete_variable(e, v)
                }
                if v.secret {
                    ui.MenuItem("Set the value of {v.key}…", icon: .key) {
                        on choose: set(target_variable, .some(v)).then(set(sheet, "secret-value"))
                    }
                }
            }
            ui.MenuSeparator()
            ui.MenuItem("Delete environment…", icon: .trash, destructive: true) {
                on choose: set(sheet, "confirm-environment")
            }
        }
        ui.MenuSeparator()
        for c in w.collections.sort_by(x -> x.name) {
            ui.MenuItem("Rename the collection {c.name}…", icon: .pencil) {
                on choose: set(target_collection, .some(c))
                    .then(set(typed_name, c.name))
                    .then(set(sheet, "rename-collection"))
            }
            ui.MenuItem("Delete the collection {c.name}…", icon: .trash, destructive: true) {
                on choose: set(target_collection, .some(c)).then(set(sheet, "confirm-collection"))
            }
        }
    }
}

/// Save into a collection: every collection is one command of the menu, and a sent request can
/// be kept as a new one.
view save_menu(w: Workspace, sheet: State<Text>, target_sent: State<Option<Sent>>) {
    ui.MenuButton("Save request", icon: .check) {
        for c in w.collections.sort_by(x -> x.name) {
            ui.MenuItem(c.name) {
                on choose: save_request(w, c)
            }
        }
        if let s = target_sent {
            ui.MenuSeparator()
            for c in w.collections.sort_by(x -> x.name) {
                ui.MenuItem("The sent request into {c.name}") {
                    on choose: save_sent(w, s, c)
                }
            }
        }
        ui.MenuSeparator()
        ui.MenuItem("New collection…", icon: .plus) {
            on choose: set(sheet, "collection")
        }
    }
}

/// What can be done to the request in the builder, and to the sent one last opened.
view request_menu(
    w: Workspace,
    sheet: State<Text>,
    typed_name: State<Text>,
    target_sent: State<Option<Sent>>,
) {
    ui.MenuButton("Request actions", icon: .more_horizontal, icon_only: true) {
        ui.MenuItem("New request", icon: .plus) {
            on choose: new_request(w)
        }
        ui.MenuItem("Rename request…", icon: .pencil) {
            on choose: set(typed_name, w.builder.name).then(set(sheet, "rename-request"))
        }
        if w.opened != .none {
            ui.MenuItem("Delete the saved request…", icon: .trash, destructive: true) {
                on choose: set(sheet, "confirm-request")
            }
        }
        ui.MenuSeparator()
        ui.MenuItem("Copy request", icon: .copy) {
            on choose: copy_request(w)
        }
        if let s = target_sent {
            ui.MenuItem("Forget the sent request…", icon: .trash, destructive: true) {
                on choose: delete_sent(w, s)
            }
        }
    }
}

/// The method and the address on one row.
view url_bar(w: Workspace) {
    row(gap: .related, align: .center) {
        ui.EnumField(bind: w.builder.method, label: "Method")
        ui.TextInput(bind: w.builder.url, placeholder: "Enter a URL", fill)
    }
}

/// The request being built: its tabs and the panel of the one chosen.
view request_panel(w: Workspace, tab: State<Int>) {
    ui.ViewTabs(selected: tab, label: "Request") {
        ui.Segment("Params")
        ui.Segment("Headers")
        ui.Segment("Auth")
        ui.Segment("Body")
        on change(chosen): set(tab, chosen)
    }
    if tab == 0 {
        ui.Section("Query parameters") {
            for entry in w.builder.params {
                parameter_row(w.builder, entry)
            }
            ui.Button("Add parameter", icon: .plus, role: .secondary, size: .small) {
                on press: add_param(w.builder)
            }
            if w.builder.params.is_empty() {
                ui.Text("No query parameters", role: .caption, secondary: true)
            }
        }
    } else if tab == 1 {
        ui.Section("Headers") {
            for entry in w.builder.headers {
                header_row(w.builder, entry)
            }
            ui.Button("Add header", icon: .plus, role: .secondary, size: .small) {
                on press: add_header(w.builder)
            }
            if w.builder.headers.is_empty() {
                ui.Text("No headers", role: .caption, secondary: true)
            }
        }
    } else if tab == 2 {
        auth_panel(w)
    } else {
        body_panel(w)
    }
}

/// One query parameter: switched on or off, its key, its value, and the control that takes it
/// off.
view parameter_row(r: Request, entry: Row) {
    row(gap: .related, align: .center) {
        ui.Checkbox(bind: entry.enabled, label: "Use this parameter")
        ui.TextInput(bind: entry.key, placeholder: "Key", fill)
        ui.TextInput(bind: entry.value, placeholder: "Value", fill)
        ui.Button("Delete parameter", icon: .x, icon_only: true, role: .plain) {
            on press: delete_param(r, entry)
        }
    }
}

/// One header: switched on or off, its name, its value, and the control that takes it off.
view header_row(r: Request, entry: Row) {
    row(gap: .related, align: .center) {
        ui.Checkbox(bind: entry.enabled, label: "Use this header")
        ui.TextInput(bind: entry.key, placeholder: "Header name", fill)
        ui.TextInput(bind: entry.value, placeholder: "Value", fill)
        ui.Button("Delete header", icon: .x, icon_only: true, role: .plain) {
            on press: delete_header(r, entry)
        }
    }
}

/// One form field: switched on or off, its key, its value, and the control that takes it off.
view form_row(r: Request, entry: Row) {
    row(gap: .related, align: .center) {
        ui.Checkbox(bind: entry.enabled, label: "Use this field")
        ui.TextInput(bind: entry.key, placeholder: "Key", fill)
        ui.TextInput(bind: entry.value, placeholder: "Value", fill)
        ui.Button("Delete field", icon: .x, icon_only: true, role: .plain) {
            on press: delete_form_field(r, entry)
        }
    }
}

/// How the request signs in: the scheme, and the fields it needs. A credential is typed into a
/// field this device keeps, never into the workspace.
view auth_panel(w: Workspace) {
    state sheet: Text = ""
    state typed: Text = ""
    ui.Section("Authentication") {
        ui.ToggleGroup(caption: "Scheme", fill: true) {
            ui.Segment("No Auth", chosen: w.builder.auth.scheme == .none) {
                on press: set_scheme(w, .none)
            }
            ui.Segment("Bearer Token", chosen: w.builder.auth.scheme == .bearer) {
                on press: set_scheme(w, .bearer)
            }
            ui.Segment("Basic Auth", chosen: w.builder.auth.scheme == .basic) {
                on press: set_scheme(w, .basic)
            }
            ui.Segment("API Key", chosen: w.builder.auth.scheme == .api_key) {
                on press: set_scheme(w, .api_key)
            }
        }
        if w.builder.auth.scheme == .none {
            ui.Text("This request does not use authentication.", role: .caption, secondary: true)
        } else if w.builder.auth.reference != "" {
            ui.Text("A sign-in is kept on this device.", role: .caption, secondary: true)
        } else {
            ui.Text("No sign-in is kept on this device yet.", role: .caption, secondary: true)
        }
        if w.builder.auth.scheme == .basic {
            ui.TextInput(bind: w.builder.auth.username, placeholder: "Username")
        }
        if w.builder.auth.scheme == .api_key {
            ui.TextInput(bind: w.builder.auth.header, placeholder: "Header name")
        }
        if w.builder.auth.scheme != .none {
            row(gap: .related) {
                ui.Button(credential_label(w.builder.auth), icon: .key) {
                    on press: set(typed, "").then(set(sheet, "credential"))
                }
                if w.builder.auth.reference != "" {
                    ui.Button("Forget sign-in", icon: .trash, role: .plain) {
                        on press: forget_credential(w)
                    }
                }
            }
        }
    }
    ui.ModalDialog(
        title: "Set the sign-in",
        open: sheet != "",
        primary: "Store",
        secondary: "Cancel",
    ) {
        body: {
            ui.TextInput(
                bind: typed,
                placeholder: credential_field(w.builder.auth.scheme),
                secret: true,
                autofocus: true,
            )
        }
        on act_primary: keep_credential(w, typed).then(set(sheet, "")).then(set(typed, ""))
        on act_secondary: set(sheet, "")
        on dismiss: set(sheet, "")
    }
}

/// What the sign-in control says, by the scheme it stores.
fn credential_label(auth: Credentials) -> Text {
    if auth.reference != "" {
        return match auth.scheme {
            .bearer => "Change the token…",
            .basic => "Change the password…",
            .api_key => "Change the key…",
            .none => "Set sign-in",
        }
    }
    match auth.scheme {
        .bearer => "Set the token…",
        .basic => "Set the password…",
        .api_key => "Set the key…",
        .none => "Set sign-in",
    }
}

/// What the sign-in sheet's field asks for.
fn credential_field(s: AuthScheme) -> Text {
    match s {
        .bearer => "Token",
        .basic => "Password",
        .api_key => "Key",
        .none => "Sign-in",
    }
}

/// The request's body: its mode, and what the mode edits.
view body_panel(w: Workspace) {
    ui.Section("Body") {
        ui.ToggleGroup(caption: "Body", fill: true) {
            ui.Segment("None", chosen: w.builder.body_mode == .none) {
                on press: set_body_mode(w, .none)
            }
            ui.Segment("JSON", chosen: w.builder.body_mode == .json) {
                on press: set_body_mode(w, .json)
            }
            ui.Segment("Form", chosen: w.builder.body_mode == .form) {
                on press: set_body_mode(w, .form)
            }
            ui.Segment("Raw", chosen: w.builder.body_mode == .raw) {
                on press: set_body_mode(w, .raw)
            }
        }
        if w.builder.body_mode == .none {
            ui.Text("This request does not have a body.", role: .caption, secondary: true)
        } else if w.builder.body_mode == .form {
            for entry in w.builder.form {
                form_row(w.builder, entry)
            }
            ui.Button("Add form field", icon: .plus, role: .secondary, size: .small) {
                on press: add_form_field(w.builder)
            }
            if w.builder.form.is_empty() {
                ui.Text("No form fields", role: .caption, secondary: true)
            }
        } else {
            ui.CodeEditor(
                bind: w.builder.body,
                language: if w.builder.body_mode == .json { .json } else { .plain_text },
                placeholder: if w.builder.body_mode == .json { "\{\}" } else { "The body" },
                min_lines: 6,
                max_lines: 14,
                gutter: false,
            )
        }
    }
}

action set_body_mode(w: Workspace, mode: BodyMode) {
    w.builder.body_mode = mode
}

/// What the service answered: how it went and how it read, its body or its headers, and the
/// commands that take it out of the app.
view response_pane(w: Workspace, response_view: State<Int>) {
    if not w.answered and not w.sending {
        ui.EmptyState(
            "No response yet",
            message: "Send the request to see what comes back.",
            icon: .globe,
            inline: true,
        )
    } else if w.sending {
        row(gap: .related, align: .center) {
            ui.Spinner(label: "Waiting for the server to respond")
            spacer
            ui.Button("Cancel request", icon: .x) {
                on press: cancel_send(w)
            }
        }
    } else if w.failure != "" {
        ui.EmptyState(
            "Couldn't complete the request",
            message: w.failure,
            icon: .triangle_alert,
            action: "Try again",
            inline: true,
            error: true,
        ) {
            on act: send(w)
        }
    } else {
        row(gap: .related, align: .center, wrap: true) {
            ui.Badge(
                "{w.answer.status} {w.answer.status_text}",
                tone: if w.answer.status >= 500 {
                    .danger
                } else if w.answer.status >= 400 {
                    .warning
                } else if w.answer.status >= 300 { .info } else { .success },
            )
            ui.Text(
                duration_text(w.answer.duration_ms),
                role: .caption,
                secondary: true,
                numeric: true,
            )
            ui.Text(size_caption(w.answer), role: .caption, secondary: true, numeric: true)
            let shown = shown_caption(w.answer)
            if shown != "" {
                ui.Text(shown, role: .caption, secondary: true, numeric: true)
            }
            spacer
            if w.notice != "" {
                ui.Text(w.notice, role: .caption, secondary: true)
                ui.Button("Dismiss", icon: .x, icon_only: true, role: .plain, size: .small) {
                    on press: dismiss_notice(w)
                }
            }
            ui.Button("Copy body", icon: .copy, icon_only: true, size: .small) {
                on press: copy_body(w)
            }
            ui.Button("Copy headers", icon: .list, icon_only: true, size: .small) {
                on press: copy_headers(w)
            }
            ui.ViewTabs(selected: response_view, label: "Response") {
                ui.Segment("Pretty")
                ui.Segment("Raw")
                ui.Segment("Headers")
                on change(chosen): set(response_view, chosen)
            }
        }
        if response_view == 2 {
            response_headers(w)
        } else if response_view == 0 {
            ui.CodeEditor(
                bind: w.answer.pretty,
                language: .json,
                read_only: true,
                gutter: true,
                min_lines: 8,
                max_lines: 30,
            )
        } else {
            ui.CodeEditor(
                bind: w.answer.body,
                language: .plain_text,
                read_only: true,
                gutter: true,
                min_lines: 8,
                max_lines: 30,
            )
        }
    }
}

/// Every header the service answered with.
view response_headers(w: Workspace) {
    ui.DataTable(name: "Response headers", fill: true) {
        columns: {
            ui.DataTableColumn("Name", kind: .identifier, width: .flex)
            ui.DataTableColumn("Value", kind: .identifier, width: .flex)
        }
        for h in w.answer.headers {
            ui.DataTableRow(h) {
                cells: {
                    ui.DataTableCell("Name", value: h.name)
                    ui.DataTableCell("Value", value: h.value)
                }
            }
        }
    }
}

/// The sheets of the window: creating, renaming, adding a variable, and the questions asked
/// before something is taken away.
// ---------------------------------------------------------------------------
// The sheets that collect a name, a key or a value
// ---------------------------------------------------------------------------
view sheets(
    w: Workspace,
    sheet: State<Text>,
    typed_name: State<Text>,
    typed_key: State<Text>,
    typed_secret: State<Text>,
    form_secret: State<Bool>,
    target_collection: State<Option<SavedCollection>>,
    target_variable: State<Option<Variable>>,
) {
    ui.ModalDialog(
        title: "New collection",
        open: sheet == "collection",
        primary: "Create",
        secondary: "Cancel",
    ) {
        body: {
            ui.TextInput(bind: typed_name, placeholder: "Collection name", autofocus: true)
        }
        on act_primary: new_collection(w, typed_name).then(set(sheet, "")).then(set(typed_name, ""))
        on act_secondary: set(sheet, "")
        on dismiss: set(sheet, "")
    }
    ui.ModalDialog(
        title: "Rename collection",
        open: sheet == "rename-collection",
        primary: "Rename",
        secondary: "Cancel",
    ) {
        body: {
            ui.TextInput(bind: typed_name, placeholder: "Collection name", autofocus: true)
        }
        on act_primary: match target_collection {
            .some(c) => rename_collection(c, typed_name)
                .then(set(sheet, ""))
                .then(set(typed_name, "")),
            .none => set(sheet, ""),
        }
        on act_secondary: set(sheet, "")
        on dismiss: set(sheet, "")
    }
    ui.ModalDialog(
        title: "Delete the collection?",
        open: sheet == "confirm-collection",
        primary: "Delete",
        secondary: "Cancel",
        destructive: true,
    ) {
        body: {
            ui.Text(collection_warning())
        }
        on act_primary: match target_collection {
            .some(c) => delete_collection(w, c).then(set(sheet, "")),
            .none => set(sheet, ""),
        }
        on act_secondary: set(sheet, "")
        on dismiss: set(sheet, "")
    }
    ui.ModalDialog(
        title: "New environment",
        open: sheet == "environment",
        primary: "Create",
        secondary: "Cancel",
    ) {
        body: {
            ui.TextInput(bind: typed_name, placeholder: "Environment name", autofocus: true)
        }
        on act_primary: new_environment(w, typed_name)
            .then(set(sheet, ""))
            .then(set(typed_name, ""))
        on act_secondary: set(sheet, "")
        on dismiss: set(sheet, "")
    }
    ui.ModalDialog(
        title: "Rename environment",
        open: sheet == "rename-environment",
        primary: "Rename",
        secondary: "Cancel",
    ) {
        body: {
            ui.TextInput(bind: typed_name, placeholder: "Environment name", autofocus: true)
        }
        on act_primary: if let e = w.active_environment {
            rename_environment(e, typed_name).then(set(sheet, "")).then(set(typed_name, ""))
        } else { set(sheet, "") }
        on act_secondary: set(sheet, "")
        on dismiss: set(sheet, "")
    }
    ui.ModalDialog(
        title: "Delete the environment?",
        open: sheet == "confirm-environment",
        primary: "Delete",
        secondary: "Cancel",
        destructive: true,
    ) {
        body: {
            ui.Text(environment_warning(w))
        }
        on act_primary: delete_active_environment(w).then(set(sheet, ""))
        on act_secondary: set(sheet, "")
        on dismiss: set(sheet, "")
    }
    ui.ModalDialog(
        title: "Rename request",
        open: sheet == "rename-request",
        primary: "Rename",
        secondary: "Cancel",
    ) {
        body: {
            ui.TextInput(bind: typed_name, placeholder: "Request name", autofocus: true)
        }
        on act_primary: rename_draft(w, typed_name).then(set(sheet, "")).then(set(typed_name, ""))
        on act_secondary: set(sheet, "")
        on dismiss: set(sheet, "")
    }
    ui.ModalDialog(
        title: "Delete the saved request?",
        open: sheet == "confirm-request",
        primary: "Delete",
        secondary: "Cancel",
        destructive: true,
    ) {
        body: {
            ui.Text("The saved request is deleted. This cannot be undone.")
        }
        on act_primary: delete_opened_request(w).then(set(sheet, ""))
        on act_secondary: set(sheet, "")
        on dismiss: set(sheet, "")
    }
    ui.ModalDialog(
        title: "Add variable",
        open: sheet == "variable",
        primary: "Create",
        secondary: "Cancel",
    ) {
        body: {
            ui.TextInput(bind: typed_key, placeholder: "Variable key", autofocus: true)
            ui.Checkbox(bind: form_secret, label: "Its value is a credential kept on each device")
        }
        on act_primary: if let e = w.active_environment {
            add_variable(e, typed_key, form_secret).then(set(sheet, "")).then(set(typed_key, ""))
        } else { set(sheet, "") }
        on act_secondary: set(sheet, "")
        on dismiss: set(sheet, "")
    }
    ui.ModalDialog(
        title: "Variables",
        open: sheet == "variables",
        primary: "Done",
        secondary: "Cancel",
    ) {
        body: {
            if let e = w.active_environment {
                for v in e.variables.sort_by(x -> x.key) {
                    variable_row(e, v, sheet, typed_secret, target_variable)
                }
                if e.variables.is_empty() {
                    ui.Text("No variables yet", role: .caption, secondary: true)
                }
            }
        }
        on act_primary: set(sheet, "")
        on act_secondary: set(sheet, "")
        on dismiss: set(sheet, "")
    }
    ui.ModalDialog(
        title: "Set the value",
        open: sheet == "secret-value",
        primary: "Store",
        secondary: "Cancel",
    ) {
        body: {
            ui.TextInput(bind: typed_secret, placeholder: "Value", secret: true, autofocus: true)
        }
        on act_primary: match target_variable {
            .some(v) => keep_variable(v, typed_secret)
                .then(set(sheet, ""))
                .then(set(typed_secret, "")),
            .none => set(sheet, ""),
        }
        on act_secondary: set(sheet, "")
        on dismiss: set(sheet, "")
    }
}

/// One variable in the environment's sheet: its key, its value (kept on the device when it is a
/// secret), whether it fills what is sent, and the control that takes it off the environment.
view variable_row(
    e: Environment,
    v: Variable,
    sheet: State<Text>,
    typed_secret: State<Text>,
    target_variable: State<Option<Variable>>,
) {
    row(gap: .related, align: .center) {
        ui.Checkbox(bind: v.enabled, label: "Use this variable")
        ui.TextInput(bind: v.key, placeholder: "Variable key", fill)
        if v.secret {
            ui.Text(
                if v.reference == "" { "No value kept" } else { "Kept on this device" },
                role: .caption,
                secondary: true,
            )
            ui.Button("Set the value…", icon: .key) {
                on press: set(target_variable, .some(v))
                    .then(set(typed_secret, ""))
                    .then(set(sheet, "secret-value"))
            }
        } else {
            ui.TextInput(bind: v.value, placeholder: "Variable value", fill)
        }
        ui.Button("Delete variable", icon: .x, icon_only: true, role: .plain) {
            on press: delete_variable(e, v)
        }
    }
}

/// What a collection being deleted holds.
fn collection_warning() -> Text {
    "The collection and the requests saved in it are deleted. This cannot be undone."
}

/// What the environment a person is about to delete holds.
fn environment_warning(w: Workspace) -> Text {
    if let e = w.active_environment {
        return "{e.name} and its variables are deleted. This cannot be undone."
    }
    "The environment is deleted. This cannot be undone."
}

/// `text` as JSON reads, indented two spaces, with its numbers, strings and literals as they
/// are; the same text when it is not JSON.
// ---------------------------------------------------------------------------
// Pretty JSON
// ---------------------------------------------------------------------------
fn pretty_json(text: Text) -> Text {
    match json.parse(text) {
        .ok(value) => written(value, 0),
        .err(why) => text,
    }
}

/// `value` written out, indented at `depth`.
fn written(value: json.Value, depth: Int) -> Text {
    match value {
        .object(fields) => {
            let names = fields.keys()
            if names.is_empty() {
                return "\{\}"
            }
            let lines = names.map(
                n -> "{spaces(depth + 1)}{json.encode(n)}: {field_written(fields, n, depth + 1)}",
            )
            "\{\n{lines.join(",\n")}\n{spaces(depth)}\}"
        }
        .array(items) => {
            if items.is_empty() {
                return "[]"
            }
            let lines = items.map(x -> "{spaces(depth + 1)}{written(x, depth + 1)}")
            "[\n{lines.join(",\n")}\n{spaces(depth)}]"
        }
        .string(text) => json.encode(text),
        .number(n) => number_text(n),
        .bool(flag) => if flag { "true" } else { "false" },
        .null => "null",
    }
}

/// `fields`' entry `name` written out, or `null` where the document held none.
fn field_written(fields: Map<json.Value>, name: Text, depth: Int) -> Text {
    match fields.get(name) {
        .some(value) => written(value, depth),
        .none => "null",
    }
}

/// A number as JSON writes it: whole numbers without a decimal part.
fn number_text(n: Float) -> Text {
    if n.fract() == 0.0 and n.abs() < 1000000000000000.0 {
        return n.to_int().to_text()
    }
    n.to_text()
}

/// `depth` levels of two spaces.
fn spaces(depth: Int) -> Text {
    if depth <= 0 {
        return ""
    }
    "  " + spaces(depth - 1)
}

/// A collection of requests against a users API, the environment its variables live in, and what
/// came back the last time one was sent.
// ---------------------------------------------------------------------------
// The sample workspace the walk and the screenshots show
// ---------------------------------------------------------------------------
scenario "a users API" {
    let api = tester.collections.insert(SavedCollection(name: "Users API"))
    let list = api.requests.insert(Request(name: "List users", url: "\{\{base\}\}/users"))
    list.method = .get
    list.headers.insert(Row(key: "Accept", value: "application/json"))
    list.params.insert(Row(key: "page", value: "1"))
    let create = api.requests.insert(Request(name: "Create user", url: "\{\{base\}\}/users"))
    create.method = .post
    create.body_mode = .json
    create.body = "\{\"name\": \"Ada\"\}"
    create.headers.insert(Row(key: "Content-Type", value: "application/json"))
    let env = tester.environments.insert(Environment(name: "Staging"))
    env.variables.insert(Variable(key: "base", value: "https://api.example.com"))
    tester.active_environment = .some(env)
    tester.builder.name = list.name
    tester.builder.method = list.method
    tester.builder.url = list.url
    for entry in list.params {
        tester.builder.params.insert(
            Row(key: entry.key, value: entry.value, enabled: entry.enabled),
        )
    }
    for entry in list.headers {
        tester.builder.headers.insert(
            Row(key: entry.key, value: entry.value, enabled: entry.enabled),
        )
    }
    tester.answer.status = 200
    tester.answer.status_text = "OK"
    tester.answer.duration_ms = 84
    tester.answer.size_bytes = 91
    tester.answer.size_exact = true
    tester.answer.shown_bytes = 91
    tester.answer.content_type = "application/json; charset=utf-8"
    tester.answer.body = "\{\"id\":4291,\"name\":\"Ada Lovelace\",\"active\":true\}"
    tester.answer.pretty = "\{\n  \"id\": 4291,\n  \"name\": \"Ada Lovelace\",\n  \"active\": true\n\}"
    tester.answer.headers.insert(
        AnswerHeader(name: "content-type", value: "application/json; charset=utf-8"),
    )
    tester.answer.headers.insert(AnswerHeader(name: "server", value: "tilo-edge/2.4"))
    tester.answered = true
}
