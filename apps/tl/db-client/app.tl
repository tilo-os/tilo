app so.tilo.db-client "Database Client" (
    icon: .database,
    category: .development,
    tagline: "Connect to databases and run SQL",
    keywords: ["database", "databases", "sql", "sqlite", "postgres", "postgresql", "mysql", "query", "queries", "tables", "table", "datenbank", "datenbanken", "abfrage", "abfragen", "tabellen"],
    splash_budget: 250,
)

op "open"
op "close"
op "query"
op "keys"
op "indexes"
op "browse"
op "catalog"
op "tables"
op "columns"

/// The engine a connection targets.
enum Engine {
    /// A file on this device.
    sqlite
    /// A PostgreSQL server.
    postgres
    /// A MySQL server.
    mysql
}

/// Which of the connection's three views the window shows.
enum Tab {
    /// The SQL editor and the result of its last statement.
    query
    /// One page of the selected table's rows.
    data
    /// The selected table's columns, keys and indexes.
    structure
}

/// One saved connection: where it goes and what it signs in with. Its password is never here,
/// so a member shares a team's databases without sharing a secret.
type Connection (title: name) {
    name: Text = "Untitled"
    engine: Engine = .postgres
    host: Text = "localhost"
    port: Int = 5432
    database: Text = ""
    user: Text = ""
    /// The path of a SQLite file on this device; empty for the network engines.
    file_path: Text = ""
    /// When it was last opened, so the list says which one a person reaches for.
    used: Option<Time> = .none
}

/// A statement a member saved to run again.
type SavedQuery (title: name) {
    name: Text = "Untitled query"
    sql: Text = ""
}

/// The workspace's databases: the connections every member sees, the queries they saved, and the
/// statement the editor holds.
type Database {
    connections: Collection<Connection>
    queries: Collection<SavedQuery>
    /// The SQL the editor holds, shared so two people work one statement together.
    sql: Text = ""
    /// Each member's own window on this device's databases.
    sessions: Collection<Session>
}

root database: Database

default view window for Database

/// One table or view of an open connection's catalog.
// ---------------------------------------------------------------- what the engine answers
type TableInfo (title: name) {
    schema: Text = "main"
    name: Text = ""
    /// `table` or `view`.
    kind: Text = "table"
    /// The rows a `COUNT(*)` found; nothing for a view.
    row_count: Option<Int> = .none
}

/// One column of a table or view of the open connection's catalog, with the relation it is in:
/// search finds it by its name and says where it is.
type ColumnInfo (title: name) {
    schema: Text = "main"
    table: Text = ""
    name: Text = ""
    decl_type: Text = ""
    nullable: Bool = true
    primary_key: Bool = false
    default_value: Option<Text> = .none
}

type KeyInfo {
    column: Text = ""
    ref_table: Text = ""
    ref_column: Text = ""
}

type IndexInfo {
    name: Text = ""
    unique: Bool = false
    columns: Text = ""
}

/// What a statement answered: its columns, every row's text, which cells hold nothing and which
/// columns align as numbers.
type Answer {
    columns: List<Text>
    rows: List<List<Text>>
    nulls: List<List<Bool>>
    numeric: List<Bool>
    rows_affected: Option<Int> = .none
    is_select: Bool = false
    elapsed_ms: Float = 0.0
    /// `rows` or `payload` when collection stopped short, empty when the whole result is here.
    truncated: Text = ""
}

/// How far a job has got, as the device answers it.
type Progress {
    /// What the statement answered, as JSON, once it is done.
    result: Text = ""
}

/// What a connection answered when it opened: the handle this device's engine gives it.
type Opened {
    connection: Int = 0
}

/// A password this window keeps for a connection. It stays out of the shared connection the
/// members see, so a connection is shared without its secret, and the password is concealed:
/// search never holds or finds it.
// ---------------------------------------------------------------- this device's window
type Secret {
    connection: Text = ""
    password: Text = ""

    search {
        conceal password
    }
}

/// A statement this window ran, one press away again.
type Run (title: summary, untitled: "Query") {
    connection: Text = ""
    sql: Text = ""
    summary: Text = ""
}

/// One row of a result: its cells as the grid shows them, and which of them hold nothing.
type Row {
    cells: List<Text>
    nulls: List<Bool>
}

/// What a statement or a table page answered, with the words the window shows of it.
type Outcome (title: summary, untitled: "Result") {
    seq: Int merge counter
    columns: List<Text>
    numeric: List<Bool>
    /// Rows a statement changed; -1 for one that returns rows.
    affected: Int = -1
    is_select: Bool = false
    elapsed_ms: Float = 0.0
    truncated: Text = ""
    /// The words the window says of it.
    summary: Text = ""
    /// The column the person ordered it by; empty for the statement's own order.
    order: Text = ""
    descending: Bool = false
}

/// One person's window on this device's databases: the connection it has open, what the catalog
/// holds, which view it shows and what its last statements answered. It is the reader's own, so
/// two people at one workspace never turn each other's pages, and search finds it by the name of
/// the database it has open.
type Session (title: name, untitled: "Database window", readers: author) {
    /// The saved connection this device opened, by its id; empty while none is.
    open: Text = ""
    name: Text = ""
    connected: Bool = false
    /// The handle this device's engine gives the open connection.
    handle: Int = 0
    tab: Tab = .query
    /// The schema names the open connection's catalog holds, and its tables.
    schemas: List<Text>
    tables: Collection<TableInfo>
    /// Every relation's columns, read with the catalog, so search finds each one and the
    /// structure view has the selected table's.
    catalog_columns: Collection<ColumnInfo>
    keys: Collection<KeyInfo>
    indexes: Collection<IndexInfo>
    selected_schema: Text = ""
    selected_table: Text = ""
    /// The row this window shows first, walked page by page, and how many a page holds.
    offset: Int merge register = 0
    page: Int = 100
    /// What the window says of the last thing it did; empty while it says nothing.
    status: Text = ""
    /// Why the last thing did not work; empty while nothing went wrong.
    notice: Text = ""
    /// The job in flight; empty while nothing runs.
    busy: Text = ""
    /// `open` or `query`, so the job's answer is read as what it is.
    job_kind: Text = ""
    /// A statement that changes data, waiting for the person to confirm it.
    pending: Text = ""
    confirming: Bool = false
    /// A statement's result and a table's page, each kept in its own record, and their rows.
    queries: Collection<Outcome>
    pages: Collection<Outcome>
    result_rows: Collection<Row>
    page_rows: Collection<Row>
    /// What this window ran, so it is one press away again.
    history: Collection<Run>
    /// Whether a transaction is open on the connection: what runs until the person commits or
    /// rolls it back.
    transaction: Bool = false
    /// The statement the job in flight runs, so its answer can be read as what it is.
    started_sql: Text = ""
    /// What this window keeps of the connections' passwords.
    secrets: Collection<Secret>
    /// The words the window says while a job runs.
    working: Text = ""
}

// ---------------------------------------------------------------- names, defaults and what a cell says
fn engine_key(e: Engine) -> Text {
    if e == .sqlite { "sqlite" } else if e == .mysql { "mysql" } else { "postgres" }
}

fn default_port(e: Engine) -> Int {
    if e == .sqlite { 0 } else if e == .mysql { 3306 } else { 5432 }
}

fn engine_of(index: Int) -> Engine {
    if index == 0 { .sqlite } else if index == 2 { .mysql } else { .postgres }
}

fn tab_index(tab: Tab) -> Int {
    if tab == .data { 1 } else if tab == .structure { 2 } else { 0 }
}

/// The address a connection reaches: `host:port`, or the file's path.
fn address(c: Connection) -> Text {
    if c.engine == .sqlite {
        if c.file_path == "" { "No file chosen" } else { c.file_path }
    } else if c.port == default_port(c.engine) { "{c.host}" } else { "{c.host}:{c.port}" }
}

/// The password this window keeps for `connection`.
fn password_of(s: Session, connection: Text) -> Text {
    s.secrets.find(x -> x.connection == connection)?.password ?? ""
}

/// What a result says of itself: its rows and how long they took.
fn say_result(a: Answer) -> Text {
    let ms = if a.elapsed_ms < 1.0 { "under a millisecond" } else { "{a.elapsed_ms.round()} ms" }
    if not a.is_select {
        "Changed {a.rows_affected ?? 0} in {ms}"
    } else if a.truncated != "" {
        "The first {a.rows.len()} rows in {ms}"
    } else { "{a.rows.len()} in {ms}" }
}

/// What the window says about a result that was cut short, or nothing when it was whole.
fn cut_note(truncated: Text) -> Text {
    if truncated == "rows" {
        "This result was cut at 1,000 rows. Narrow the query, or page through the table instead."
    } else if truncated == "payload" {
        "This result was cut to keep the window responsive: 8 MB is as much of it as is held. Narrow the query to see the rest."
    } else { "" }
}

/// One cell's text, by its column's place in the row.
fn cell_at(row: Row, at: Int) -> Text {
    row.cells.at(at) ?? ""
}

/// Whether the cell at `at` of `row` holds nothing.
fn cell_empty(row: Row, at: Int) -> Bool {
    row.nulls.at(at) ?? false
}

/// Whether a column aligns as a number.
fn numeric_column(numeric: List<Bool>, at: Int) -> Bool {
    numeric.at(at) ?? false
}

/// One row as tab-separated text: `NULL` is written as nothing, and a tab or newline inside a
/// cell becomes a space so the row stays one line.
fn row_text(o: Outcome, row: Row) -> Text {
    let cells: List<Text> = []
    let place = 0
    for c in o.columns {
        if cell_empty(row, place) {
            cells = cells + [""]
        } else {
            cells = cells
                + [cell_at(row, place).replace("\t", " ").replace("\n", " ").replace("\r", " ")]
        }
        place = place + 1
    }
    cells.join("\t")
}

/// The whole result as tab-separated text, a header line first when it has columns.
fn result_text(o: Outcome, rows: List<Row>) -> Text {
    let out: List<Text> = []
    if not o.columns.is_empty() {
        out = out + [o.columns.join("\t")]
    }
    for row in rows {
        out = out + [row_text(o, row)]
    }
    out.join("\n")
}

/// The rows of `o` in the order the person asked for, by one column's text.
fn ordered_rows(rows: List<Row>, o: Outcome) -> List<Row> {
    if o.order == "" {
        rows
    } else if o.descending {
        rows.sort_by(r -> cell_at(r, o.columns.index_of(o.order) ?? 0), desc)
    } else {
        rows.sort_by(r -> cell_at(r, o.columns.index_of(o.order) ?? 0))
    }
}

/// The verbs that change data, which ask before they run.
// ---------------------------------------------------------------- the SQL safety check
fn changing_verb(word: Text) -> Bool {
    [
        "ALTER",
        "CREATE",
        "DELETE",
        "DROP",
        "INSERT",
        "REPLACE",
        "TRUNCATE",
        "UPDATE",
        "MERGE",
        "GRANT",
        "REVOKE",
        "WITH",
    ]
        .any(v -> v == word)
}

/// Whether running `sql` should ask first: a statement that changes data, a script of several
/// statements, or one whose verb this client cannot read. `SELECT` and `EXPLAIN` run at once.
fn asks_first(sql: Text) -> Bool {
    let statements = sql.split(";").filter(part -> part.trim() != "").count()
    if statements != 1 {
        return true
    }
    changing_verb(first_word(sql))
}

/// The statement's verb: the first word of the code, with the lines a comment makes skipped.
fn first_word(sql: Text) -> Text {
    let code: List<Text> = []
    for line in sql.split("\n") {
        let trimmed = line.trim()
        if trimmed != "" and not trimmed.starts_with("--") {
            code = code + [trimmed]
        }
    }
    let held = ""
    for word in code.join(" ").trim().upper().split(" ") {
        if held == "" and word != "" and not word.starts_with("/*") and word != "*/" {
            held = word
        }
    }
    held
}

/// A CSV field safe to open in spreadsheet software: a leading formula sigil stays data,
/// prefixed with an apostrophe, and every field is quoted.
fn csv_field(value: Text) -> Text {
    let trimmed = value.trim()
    let formula = trimmed.starts_with("=")
        or trimmed.starts_with("+")
        or trimmed.starts_with("-")
        or trimmed.starts_with("@")
    let safe = if formula { "'{value}" } else { value }
    "\"{safe.replace("\"", "\"\"")}\""
}

/// The result as a CSV file's text, a header line first.
fn csv_text(o: Outcome, rows: List<Row>) -> Text {
    let lines: List<Text> = []
    if not o.columns.is_empty() {
        lines = lines + [o.columns.map(c -> csv_field(c)).join(",")]
    }
    for row in rows {
        let cells: List<Text> = []
        let place = 0
        for c in o.columns {
            cells = cells + [csv_field(cell_at(row, place))]
            place = place + 1
        }
        lines = lines + [cells.join(",")]
    }
    "{lines.join("\n")}\n"
}

fn first_line(sql: Text) -> Text {
    let line = sql.split("\n").first() ?? "Saved query"
    if line.trim() == "" { "Saved query" } else { line.trim() }
}

/// This person's window, made by their own first action so it is theirs to read and write.
// ---------------------------------------------------------------- what a person asks for
action ensure_session(d: Database) -> Session {
    d.sessions.first() ?? d.sessions.insert(Session(schemas: []))
}

/// Whether this person's window shows an open connection.
fn session_is_connected(d: Database) -> Bool {
    if let s = d.sessions.first() { s.connected } else { false }
}

/// Whether this person's window has a job in flight.
fn session_is_busy(d: Database) -> Bool {
    if let s = d.sessions.first() { s.busy != "" } else { false }
}

/// Whether this person's window has a transaction open.
fn in_transaction(d: Database) -> Bool {
    if let s = d.sessions.first() { s.transaction } else { false }
}

/// The row the window shows first.
fn session_offset(d: Database) -> Int {
    if let s = d.sessions.first() { s.offset } else { 0 }
}

/// The result in front, when the window has one.
fn session_queries(d: Database) -> Option<Outcome> {
    if let s = d.sessions.first() { s.queries.first() } else { .none }
}

/// Opens a saved connection: this device starts the engine on it and reads its catalog.
action connect(d: Database, c: Connection) uses time {
    let s = ensure_session(d)
    if s.busy != "" {
        s.notice = "Another database operation is already running"
        return
    }
    if c.engine == .sqlite and c.file_path == "" {
        s.notice = "This SQLite connection has no file: edit it and choose one"
        return
    }
    let ask = json.encode(
        OpenAsk(
            engine: engine_key(c.engine),
            host: c.host,
            port: c.port,
            database: c.database,
            user: c.user,
            password: password_of(s, c.id),
            file_path: c.file_path,
        ),
    )
    s.open = c.id
    s.name = c.name
    s.connected = false
    s.handle = 0
    s.schemas = []
    s.selected_schema = ""
    s.selected_table = ""
    s.offset = 0
    s.notice = ""
    s.status = ""
    s.working = "Connecting to {c.name}…"
    c.used = .some(now)
    match engine.start("open", ask, done: j -> job_done(d, j)) {
        .ok(job) => {
            s.busy = job
            s.job_kind = "open"
        }
        .err(why) => {
            s.working = ""
            s.notice = why
        }
    }
}

/// Closes the open connection on this device.
action disconnect(d: Database) {
    let s = ensure_session(d)
    if s.busy != "" {
        s.notice = "A database operation is still running"
        return
    }
    if s.handle != 0 {
        engine.call("close", json.encode(Handle(connection: s.handle)))
    }
    s.handle = 0
    s.connected = false
    s.transaction = false
    s.started_sql = ""
    s.schemas = []
    s.selected_schema = ""
    s.selected_table = ""
    // A closed connection's catalog is not the open one's: nothing of it is kept to be found.
    clear_tables(s)
    clear_columns(s)
    s.open = ""
    s.name = ""
    s.status = ""
    s.working = ""
    s.notice = ""
}

/// Adds a connection to the workspace, keeps its password on this device, and opens it.
action add_connection(
    d: Database,
    name: Text,
    engine_index: Int,
    host: Text,
    port: Int,
    database: Text,
    user: Text,
    file_path: Text,
    password: Text,
) uses time {
    let trimmed = name.trim()
    let target = engine_of(engine_index)
    let made = d.connections.insert(
        Connection(
            name: if trimmed == "" { "Untitled" } else { trimmed },
            engine: target,
            host: if host.trim() == "" { "localhost" } else { host.trim() },
            port: if port > 0 { port } else { default_port(target) },
            database: database.trim(),
            user: user.trim(),
            file_path: file_path.trim(),
        ),
    )
    remember_password(d, made, password)
    connect(d, made)
}

/// Removes a saved connection: its password goes with it on this device.
action remove_connection(d: Database, c: Connection) {
    let s = ensure_session(d)
    if s.open == c.id { disconnect(d) }
    forget_password(s, c)
    d.connections.remove(c)
}

/// Drops what this device kept for `c`.
action forget_password(s: Session, c: Connection) {
    if let held = s.secrets.find(x -> x.connection == c.id) { s.secrets.remove(held) }
}

/// Keeps `password` for `c` on this device, or forgets it when it is empty.
action remember_password(d: Database, c: Connection, password: Text) {
    let s = ensure_session(d)
    if let held = s.secrets.find(x -> x.connection == c.id) {
        if password == "" {
            s.secrets.remove(held)
        } else {
            held.password = password
        }
    } else if password != "" {
        s.secrets.insert(Secret(connection: c.id, password: password))
    }
}

/// Saves the statement in the editor under `name`.
action save_query(d: Database, name: Text) uses time {
    require d.sql.trim() != "" "Write a statement before saving it"
    let trimmed = name.trim()
    d.queries.insert(
        SavedQuery(name: if trimmed == "" { "Untitled query" } else { trimmed }, sql: d.sql),
    )
}

/// Removes a saved query.
action remove_query(d: Database, q: SavedQuery) {
    d.queries.remove(q)
}

/// Loads a saved query into the editor, ready to run.
action load_query(d: Database, q: SavedQuery) {
    d.sql = q.sql
    show_tab(d, .query)
}

/// Puts a starter query in the editor, saved so the person can name it too.
action load_starter(d: Database, name: Text, sql: Text) {
    d.sql = sql
    show_tab(d, .query)
    save_query(d, name)
}

/// Shows the view the segment at `index` names. The data view reads the selected table's rows
/// and its count as it opens, so it shows the table rather than an empty pane, and a count from
/// before the last statement is read again.
action choose_tab(d: Database, index: Int) {
    if index == 1 {
        show_tab(d, .data)
        let s = ensure_session(d)
        if s.connected and s.selected_table != "" {
            refresh_counts(d, s)
            load_page(d)
        }
    } else if index == 2 { show_tab(d, .structure) } else { show_tab(d, .query) }
}

/// Reads every table's row count again, so a page read after a statement names the table's
/// count as it stands now.
action refresh_counts(d: Database, s: Session) {
    if not s.connected or s.handle == 0 {
        return
    }
    match engine.call("tables", json.encode(Handle(connection: s.handle))) {
        .ok(text) => match json.decode<List<TableInfo>>(text) {
            .ok(found) => {
                for one in found {
                    if let held = s.tables.find(t -> t.schema == one.schema and t.name == one.name) {
                        held.row_count = one.row_count
                    }
                }
            }
            .err(why) => {
                s.notice = why
            }
        },
        .err(why) => {
            s.notice = why
        }
    }
}

/// Shows one of the window's three views.
action show_tab(d: Database, tab: Tab) {
    let s = ensure_session(d)
    s.tab = tab
}

/// Starts a transaction on the open connection: what runs until the person commits is one
/// change, and rolling it back takes all of it back.
action begin_transaction(d: Database) (icon: .circle_play, label: "Begin transaction") {
    require session_is_connected(d) "Open a connection before starting a transaction"
    require not session_is_busy(d) "A query is already running"
    require not in_transaction(d) "A transaction is already open"
    start_statement(d, "BEGIN")
}

/// Commits the open transaction: what it changed is the database's from now on.
action commit_transaction(d: Database) (icon: .check, label: "Commit transaction") {
    require session_is_connected(d) "Open a connection before committing"
    require not session_is_busy(d) "A query is already running"
    require in_transaction(d) "No transaction is open"
    start_statement(d, "COMMIT")
}

/// Rolls the open transaction back: every change it made is taken back.
destructive action rollback_transaction(d: Database) (
    icon: .undo_2,
    label: "Roll back transaction",
) {
    require session_is_connected(d) "Open a connection before rolling back"
    require not session_is_busy(d) "A query is already running"
    require in_transaction(d) "No transaction is open"
    start_statement(d, "ROLLBACK")
}

/// Runs the statement in the editor, asking first when it changes data.
action run(d: Database) {
    require session_is_connected(d) "Open a connection before running SQL"
    require not session_is_busy(d) "A query is already running"
    let s = ensure_session(d)
    let sql = d.sql.trim()
    if sql == "" {
        return
    }
    if asks_first(sql) {
        s.pending = sql
        s.confirming = true
        return
    }
    start_statement(d, sql)
}

/// Runs the statement the person confirmed.
action confirm_run(d: Database) {
    let s = ensure_session(d)
    let sql = s.pending
    s.confirming = false
    s.pending = ""
    start_statement(d, sql)
}

/// Puts away the statement that was waiting for confirmation.
action dismiss_run(d: Database) {
    let s = ensure_session(d)
    s.confirming = false
    s.pending = ""
}

/// Runs a saved query against the open connection.
action run_query(d: Database, q: SavedQuery) {
    d.sql = q.sql
    show_tab(d, .query)
    run(d)
}

/// Runs a statement this window ran before.
action rerun(d: Database, r: Run) {
    d.sql = r.sql
    show_tab(d, .query)
    run(d)
}

/// Starts `sql` on the open connection as a job, so the window stays live and the statement can
/// be stopped.
action start_statement(d: Database, sql: Text) {
    let s = ensure_session(d)
    if s.handle == 0 {
        s.notice = "Open a connection before running SQL"
        return
    }
    match engine.start(
        "query",
        json.encode(QueryAsk(connection: s.handle, sql: sql)),
        done: j -> job_done(d, j),
    ) {
        .ok(job) => {
            s.busy = job
            s.job_kind = "query"
            s.started_sql = sql
            s.working = "Running…"
            s.notice = ""
        }
        .err(why) => {
            s.notice = why
        }
    }
}

/// Stops the statement in flight.
action cancel(d: Database) {
    let s = ensure_session(d)
    if s.busy == "" {
        s.status = "The query has already finished"
        return
    }
    engine.cancel(s.busy)
    s.working = "Cancelling the query…"
}

/// Opens the table `t`: what it is made of, and its first page of rows.
action browse(d: Database, t: TableInfo) {
    let s = ensure_session(d)
    if not s.connected {
        s.notice = "Open a connection before browsing its tables"
        return
    }
    s.selected_schema = t.schema
    s.selected_table = t.name
    s.offset = 0
    s.tab = .data
    load_structure(d)
    load_page(d)
}

/// Empties the tables a window holds, so a catalog read again never stands beside the one before.
action clear_tables(s: Session) {
    let old: List<TableInfo> = []
    for one in s.tables {
        old = old + [one]
    }
    for one in old {
        s.tables.remove(one)
    }
}

/// Empties the keys and indexes of the relation the window last chose.
action clear_structure(s: Session) {
    let keys: List<KeyInfo> = []
    for one in s.keys {
        keys = keys + [one]
    }
    for one in keys {
        s.keys.remove(one)
    }
    let indexes: List<IndexInfo> = []
    for one in s.indexes {
        indexes = indexes + [one]
    }
    for one in indexes {
        s.indexes.remove(one)
    }
}

/// Empties the columns a window holds, so a catalog read again never stands beside the one
/// before.
action clear_columns(s: Session) {
    let old: List<ColumnInfo> = []
    for one in s.catalog_columns {
        old = old + [one]
    }
    for one in old {
        s.catalog_columns.remove(one)
    }
}

/// Reads the selected table's keys and indexes; its columns came with the catalog.
action load_structure(d: Database) {
    let s = ensure_session(d)
    if not s.connected or s.selected_table == "" {
        return
    }
    let ask = json.encode(
        TableAsk(connection: s.handle, schema: s.selected_schema, table: s.selected_table),
    )
    clear_structure(s)
    match engine.call("keys", ask) {
        .ok(text) => match json.decode<List<KeyInfo>>(text) {
            .ok(found) => {
                for one in found {
                    s.keys.insert(one)
                }
            }
            .err(why) => {
                s.notice = why
            }
        },
        .err(why) => {
            s.notice = why
        }
    }
    match engine.call("indexes", ask) {
        .ok(text) => match json.decode<List<IndexInfo>>(text) {
            .ok(found) => {
                for one in found {
                    s.indexes.insert(one)
                }
            }
            .err(why) => {
                s.notice = why
            }
        },
        .err(why) => {
            s.notice = why
        }
    }
}

/// Loads one page of the selected table's rows.
action load_page(d: Database) {
    let s = ensure_session(d)
    if not s.connected or s.selected_table == "" {
        return
    }
    let ask = json.encode(
        BrowseAsk(
            connection: s.handle,
            schema: s.selected_schema,
            table: s.selected_table,
            limit: s.page,
            offset: s.offset,
        ),
    )
    match engine.call("browse", ask) {
        .ok(text) => match json.decode<Answer>(text) {
            .ok(answer) => {
                clear_rows(s, false)
                let at = 0
                for cells in answer.rows {
                    s.page_rows.insert(Row(cells: cells, nulls: answer.nulls.at(at) ?? []))
                    at = at + 1
                }
                let held = s.pages.first() ?? s.pages.insert(Outcome(columns: [], numeric: []))
                held.seq += 1
                held.columns = answer.columns
                held.numeric = answer.numeric
                held.affected = -1
                held.is_select = answer.is_select
                held.elapsed_ms = answer.elapsed_ms
                held.truncated = answer.truncated
                held.summary = say_result(answer)
                s.notice = ""
            }
            .err(why) => {
                s.notice = why
            }
        },
        .err(why) => {
            s.notice = why
        }
    }
}

/// Empties a window's rows, so a fresh result never stands beside the one before it.
action clear_rows(s: Session, result: Bool) {
    let old: List<Row> = []
    if result {
        for row in s.result_rows {
            old = old + [row]
        }
        for row in old {
            s.result_rows.remove(row)
        }
    } else {
        for row in s.page_rows {
            old = old + [row]
        }
        for row in old {
            s.page_rows.remove(row)
        }
    }
}

/// The rows the selected table holds, for the page controls.
fn total_rows(s: Session) -> Int {
    if let t = s.tables.find(x -> x.name == s.selected_table and x.schema == s.selected_schema) {
        t.row_count ?? 0
    } else { 0 }
}

/// Whether the selected table has a page after this one.
fn next_page_is_there(d: Database) -> Bool {
    if let s = d.sessions.first() {
        let shown = s.page_rows.len()
        total_rows(s) > s.offset + shown
    } else { false }
}

/// Shows the next page of the selected table's rows.
action next_page(d: Database, at: Int = 0) {
    require next_page_is_there(d) "This is the last page"
    let s = ensure_session(d)
    let wanted = if at > 0 { at } else { s.offset + s.page }
    s.offset = wanted
    load_page(d)
}

/// Shows the page before the one shown.
action previous_page(d: Database, at: Int = 0) {
    require session_offset(d) > 0 "This is the first page"
    let s = ensure_session(d)
    let back = if s.offset >= s.page { s.offset - s.page } else { 0 }
    s.offset = if at > 0 and at < s.offset { at } else { back }
    load_page(d)
}

/// Reads the open connection's catalog again, so a table made elsewhere shows.
action refresh_catalog(d: Database) {
    let s = ensure_session(d)
    if not s.connected or s.handle == 0 {
        return
    }
    let ask = json.encode(Handle(connection: s.handle))
    match engine.call("catalog", ask) {
        .ok(text) => match json.decode<List<Text>>(text) {
            .ok(names) => {
                s.schemas = names
            }
            .err(why) => {
                s.notice = why
            }
        },
        .err(why) => {
            s.notice = why
        }
    }
    match engine.call("tables", ask) {
        .ok(text) => match json.decode<List<TableInfo>>(text) {
            .ok(found) => {
                clear_tables(s)
                for one in found {
                    s.tables.insert(one)
                }
                s.notice = ""
                if s.selected_schema == "" {
                    if let first = s.schemas.first() {
                        s.selected_schema = first
                    }
                }
                if s.selected_table == "" {
                    if let first = found.first() {
                        s.selected_schema = first.schema
                        s.selected_table = first.name
                        load_structure(d)
                    }
                }
                // Every relation's columns, so search finds each one by its name and the
                // structure view holds the selected table's.
                let every: List<ColumnInfo> = []
                for t in found {
                    let asked = json.encode(
                        TableAsk(connection: s.handle, schema: t.schema, table: t.name),
                    )
                    match engine.call("columns", asked) {
                        .ok(text) => match json.decode<List<ColumnInfo>>(text) {
                            .ok(held) => {
                                for one in held {
                                    every = every
                                        + [
                                            ColumnInfo(
                                                schema: t.schema,
                                                table: t.name,
                                                name: one.name,
                                                decl_type: one.decl_type,
                                                nullable: one.nullable,
                                                primary_key: one.primary_key,
                                                default_value: one.default_value,
                                            ),
                                        ]
                                }
                            }
                            .err(why) => {
                                s.notice = why
                            }
                        },
                        .err(why) => {
                            s.notice = why
                        }
                    }
                }
                clear_columns(s)
                for one in every {
                    s.catalog_columns.insert(one)
                }
            }
            .err(why) => {
                s.notice = why
            }
        },
        .err(why) => {
            s.notice = why
        }
    }
}

/// Orders the result by the column the person chose, or back to the statement's own order.
action order_by(d: Database, column: Text) {
    let s = ensure_session(d)
    if let o = s.queries.first() {
        if o.order == column {
            if o.descending {
                o.order = ""
                o.descending = false
            } else {
                o.descending = true
            }
        } else {
            o.order = column
            o.descending = false
        }
    }
}

/// Copies the result row at `at` as one line of text.
action copy_row(d: Database, row: Row) uses clipboard {
    let s = ensure_session(d)
    if let o = s.queries.first() {
        if clipboard.copy(row_text(o, row)) {
            s.status = "Row copied"
        } else {
            s.status = "Could not copy to the clipboard"
        }
    }
}

/// Copies the whole result as tab-separated text.
action copy_result(d: Database) uses clipboard {
    let s = ensure_session(d)
    if let o = s.queries.first() {
        if o.columns.is_empty() {
            s.status = "Select a row to copy first"
            return
        }
        let rows: List<Row> = []
        for row in s.result_rows {
            rows = rows + [row]
        }
        if clipboard.copy(result_text(o, rows)) {
            s.status = "Result copied"
        } else {
            s.status = "Could not copy to the clipboard"
        }
    }
}

/// Whether the result in front can be written out as a CSV file.
fn exportable(d: Database) -> Bool {
    if let o = session_queries(d) { o.is_select and not o.columns.is_empty() } else { false }
}

/// The statement's result as a CSV file, for a person to keep.
action export_csv(d: Database) -> Option<File> (icon: .download, label: "Export as CSV") {
    require exportable(d) "Only a query that returns rows can be exported as CSV"
    let o = session_queries(d) ?? Outcome(columns: [], numeric: [])
    let rows: List<Row> = []
    if let s = d.sessions.first() {
        for row in s.result_rows {
            rows = rows + [row]
        }
    }
    return .some(File.of_text("query-results.csv", csv_text(o, rows)))
}

/// Clears what the window last said.
action dismiss(d: Database) {
    let s = ensure_session(d)
    s.notice = ""
    s.status = ""
}

/// The job in flight ended: its answer is read as what it is, or the window says why it failed
/// or was stopped. A job this window no longer runs leaves the window as it is.
action job_done(d: Database, j: host.Job) {
    let found = d.sessions.first()
    if let s = found {
        if s.busy != j.id {
            return
        }
        if j.state == .queued or j.state == .running {
            return
        }
        match json.decode<Progress>(j.detail) {
            .ok(progress) => {
                let kind = s.job_kind
                s.busy = ""
                s.job_kind = ""
                s.working = ""
                if j.state == .cancelled {
                    s.status = "Query cancelled"
                    s.started_sql = ""
                } else if j.state == .failed {
                    s.notice = j.error
                    if kind == "open" {
                        s.connected = false
                    }
                } else if kind == "open" {
                    opened(d, progress.result)
                } else { queried(d, progress.result) }
            }
            .err(why) => failed_job(s, why),
        }
    }
}

/// The window's job could not be asked about at all: it is over, and the window says why.
action failed_job(s: Session, why: Text) {
    s.busy = ""
    s.job_kind = ""
    s.started_sql = ""
    s.working = ""
    s.notice = why
}

/// Takes a connection's handle in, once the engine opened it, and reads its catalog.
action opened(d: Database, answer: Text) {
    let s = ensure_session(d)
    match json.decode<Opened>(answer) {
        .ok(open) => {
            s.handle = open.connection
            s.connected = true
            s.notice = ""
            s.status = "Connected to {s.name}"
            refresh_catalog(d)
        }
        .err(why) => {
            s.connected = false
            s.notice = why
        }
    }
}

/// Takes a statement's result in, with the words the window shows of it.
action queried(d: Database, answer: Text) {
    let s = ensure_session(d)
    match json.decode<Answer>(answer) {
        .ok(result) => {
            clear_rows(s, true)
            let at = 0
            for cells in result.rows {
                s.result_rows.insert(Row(cells: cells, nulls: result.nulls.at(at) ?? []))
                at = at + 1
            }
            let held = s.queries.first() ?? s.queries.insert(Outcome(columns: [], numeric: []))
            held.seq += 1
            held.columns = result.columns
            held.numeric = result.numeric
            held.affected = result.rows_affected ?? -1
            held.is_select = result.is_select
            held.elapsed_ms = result.elapsed_ms
            held.truncated = result.truncated
            held.summary = say_result(result)
            held.order = ""
            held.descending = false
            s.status = held.summary
            s.notice = ""
            s.history.insert(Run(connection: s.open, sql: d.sql, summary: held.summary))
            if s.history.len() > 20 {
                if let oldest = s.history.first() { s.history.remove(oldest) }
            }
            // A statement that begins, commits or rolls back a transaction opens or closes it;
            // any other statement leaves it as it was.
            let verb = first_word(s.started_sql)
            if verb == "BEGIN" {
                s.transaction = true
                s.status = "Transaction open"
            } else if verb == "COMMIT" {
                s.transaction = false
                s.status = "Transaction committed"
            } else if verb == "ROLLBACK" {
                s.transaction = false
                s.status = "Transaction rolled back"
            }
            s.started_sql = ""
            if not result.is_select { refresh_catalog(d) }
        }
        .err(why) => {
            s.notice = why
        }
    }
}

// ---------------------------------------------------------------- the requests the engine reads
type OpenAsk {
    engine: Text = "sqlite"
    host: Text = ""
    port: Int = 0
    database: Text = ""
    user: Text = ""
    password: Text = ""
    file_path: Text = ""
}

type QueryAsk {
    connection: Int = 0
    sql: Text = ""
}

type BrowseAsk {
    connection: Int = 0
    schema: Text = ""
    table: Text = ""
    limit: Int = 100
    offset: Int = 0
}

type TableAsk {
    connection: Int = 0
    schema: Text = ""
    table: Text = ""
}

type Handle {
    connection: Int = 0
}

/// The database client's window: the connections and the open catalog down the side, and the
/// statement, its result or the table's rows beside them. A client nobody has used yet has no
/// window state of its own, and says where to start.
// ---------------------------------------------------------------- the window
view window(d: Database) {
    let current = d.sessions.first()
    // What the person typed in the window's search: the connections and tables narrow to it.
    state found: Text = ""
    // Whether the new-connection form is open, and what the person typed in it.
    state adding: Bool = false
    state new_name: Text = ""
    state new_engine: Int = 1
    state new_host: Text = "localhost"
    state new_port: Int = 5432
    state new_database: Text = ""
    state new_user: Text = ""
    state new_file: Text = ""
    state new_password: Text = ""
    // A table system search chose opens here, showing its rows.
    on show(t: TableInfo): browse(root Database, t)
    // A saved query opens in the editor, ready to run; the window opens at any of its views.
    on show(q: SavedQuery): load_query(root Database, q)
    on show query "Query editor": show_tab(d, .query)
    on show data "Table rows": show_tab(d, .data)
    on show structure "Table structure": show_tab(d, .structure)
    let asked = found.trim().lower()
    let listed = d.connections.filter(c -> asked == "" or c.name.lower().contains(asked))
    let tables = listing(current, asked)
    ui.NavShell(
        content_kind: .document,
        title: current?.name ?? "Database",
        count: if current?.connected ?? false { tables.len() } else { listed.len() },
    ) {
        on search(t: Text): set(found, t)
        sidebar: {
            if let s = current {
                navigator(d, s, listed, tables)
            } else {
                ui.NavList(selected: "") {
                    ui.NavSection("Connections") {
                        for c in listed {
                            ui.NavItem(
                                c.id,
                                c.name,
                                icon: if c.engine == .sqlite { .hard_drive } else { .globe },
                            ) {
                                on select: connect(d, c)
                            }
                        }
                    }
                }
            }
        }
        actions: {
            ui.Button("New connection", icon: .plus, role: .primary) {
                on press: set(adding, true)
                    .then(set(new_name, ""))
                    .then(set(new_engine, 1))
                    .then(set(new_host, "localhost"))
                    .then(set(new_port, 5432))
                    .then(set(new_database, ""))
                    .then(set(new_user, ""))
                    .then(set(new_file, ""))
                    .then(set(new_password, ""))
            }
        }
        content: {
            stack {
                if let s = current {
                    if s.notice != "" {
                        ui.Banner(
                            "That did not work",
                            message: s.notice,
                            kind: .error,
                            icon: .triangle_alert,
                            action: "Dismiss",
                        ) {
                            on act: dismiss(d)
                        }
                    }
                }
                if d.connections.is_empty() {
                    ui.EmptyState(
                        "No connections yet",
                        icon: .database,
                        message: "Add a PostgreSQL, MySQL or SQLite database to browse its tables and run queries.",
                        action: "New connection",
                    ) {
                        on act: set(adding, true)
                    }
                } else if let s = current {
                    ui.ViewTabs(selected: tab_index(s.tab), label: "View") {
                        ui.Segment("Query", icon: .file_code)
                        ui.Segment("Data", icon: .table)
                        ui.Segment("Structure", icon: .key)
                        on change(i): choose_tab(d, i)
                    }
                    if s.tab == .query {
                        query_tab(d, s)
                    } else if s.tab == .data {
                        data_tab(d, s)
                    } else {
                        structure_tab(s)
                    }
                    if s.confirming {
                        confirm_dialog(d, s)
                    }
                } else {
                    ui.EmptyState(
                        "Choose a connection to open",
                        icon: .plug,
                        message: "Opening one reads its tables and signs in with what this device keeps.",
                    )
                }
                if adding {
                    connect_dialog(
                        d,
                        adding,
                        new_name,
                        new_engine,
                        new_host,
                        new_port,
                        new_database,
                        new_user,
                        new_file,
                        new_password,
                    )
                }
            }
        }
    }
}

/// The tables the window lists: the open connection's, narrowed by what the person searched.
fn listing(current: Option<Session>, asked: Text) -> List<TableInfo> {
    if let s = current {
        s.tables.filter(t -> asked == "" or t.name.lower().contains(asked))
    } else { [] }
}

/// The connections, the open catalog, the queries this workspace saved and what this window ran.
view navigator(d: Database, s: Session, listed: List<Connection>, tables: List<TableInfo>) {
    ui.NavList(
        selected: if s.selected_table != "" {
            s.selected_schema + "." + s.selected_table
        } else { s.open },
    ) {
        ui.NavSection("Connections") {
            for c in listed {
                ui.NavItem(
                    c.id,
                    c.name,
                    icon: if c.engine == .sqlite { .hard_drive } else { .globe },
                ) {
                    on select: connect(d, c)
                }
            }
        }
        if s.connected {
            ui.NavSection(s.name) {
                if tables.is_empty() {
                    ui.NavItem("no-tables", "No tables", icon: .table) {
                        on select: refresh_catalog(d)
                    }
                }
                for t in tables {
                    ui.NavItem(
                        "{t.schema}.{t.name}",
                        t.name,
                        icon: if t.kind == "view" { .eye } else { .table },
                        count: t.row_count ?? 0,
                    ) {
                        on select: browse(d, t)
                    }
                }
            }
        }
        if not d.queries.is_empty() {
            ui.NavSection("Saved queries") {
                for q in d.queries {
                    ui.NavItem(q.id, q.name, icon: .file_code) {
                        on select: load_query(d, q)
                    }
                }
            }
        }
        if not s.history.is_empty() {
            ui.NavSection("Run in this window") {
                for r in s.history {
                    ui.NavItem(r.id, r.summary, icon: .clock) {
                        on select: rerun(d, r)
                    }
                }
            }
        }
    }
}

/// The statement, the way to run it, and what it answered.
view query_tab(d: Database, s: Session) {
    stack(gap: .related) {
        row(gap: .related, align: .center) {
            starter_queries(d)
            spacer
            if s.transaction {
                ui.Badge("Transaction open", tone: .warning)
            }
            if s.busy != "" {
                ui.Spinner(label: if s.working == "" { "Running…" } else { s.working })
                ui.Button("Cancel query", icon: .x) {
                    on press: cancel(d)
                }
            } else {
                ui.Button("Run query", icon: .play, role: .primary) {
                    on press: run(d)
                }
                if s.transaction {
                    ui.Button("Commit transaction", icon: .check) {
                        on press: commit_transaction(d)
                    }
                    ui.Button("Roll back transaction", icon: .undo_2, role: .destructive) {
                        on press: rollback_transaction(d)
                    }
                } else if s.connected {
                    ui.Button("Begin transaction", icon: .circle_play) {
                        on press: begin_transaction(d)
                    }
                }
            }
            result_menu(d, s)
        }
        ui.TextInput(bind: d.sql, placeholder: "Write a SQL query", lines: 6) {
        }
        if s.status != "" {
            ui.Text(s.status, role: .caption, secondary: true)
        }
        if let o = s.queries.first() {
            if cut_note(o.truncated) != "" {
                ui.Text(cut_note(o.truncated), role: .caption, secondary: true)
            }
            result_grid(d, o, s.result_rows)
        } else {
            ui.EmptyState("Write a statement and run it", icon: .play, inline: true)
        }
    }
}

/// What can be done with the result in front.
view result_menu(d: Database, s: Session) {
    ui.MenuButton("Result", icon: .ellipsis, icon_only: true) {
        ui.MenuItem("Copy result", icon: .clipboard_paste) {
            on choose: copy_result(d)
        }
        ui.MenuItem("Export as CSV…", icon: .download, disabled: not exportable(d)) {
            on choose: file.save(export_csv(d), name: "query-results.csv")
        }
        ui.MenuItem("Save the query…", icon: .save, disabled: d.sql.trim() == "") {
            on choose: save_query(d, first_line(d.sql))
        }
        if let o = s.queries.first() {
            for c in o.columns {
                ui.MenuItem("Order by {c}", checked: o.order == c) {
                    on choose: order_by(d, c)
                }
            }
        }
        ui.MenuItem("Disconnect", icon: .x, disabled: not s.connected) {
            on choose: disconnect(d)
        }
        ui.MenuItem("Read the catalog again", icon: .refresh, disabled: not s.connected) {
            on choose: refresh_catalog(d)
        }
    }
}

/// The queries a new person starts from, one press from the editor.
view starter_queries(d: Database) {
    ui.MenuButton("Starter queries", icon: .list) {
        ui.MenuItem("Top tracks by price") {
            on choose: load_starter(
                d,
                "Top tracks by price",
                "SELECT name, unit_price FROM tracks ORDER BY unit_price DESC LIMIT 10;",
            )
        }
        ui.MenuItem("Albums with artist") {
            on choose: load_starter(
                d,
                "Albums with artist",
                "SELECT albums.title, artists.name FROM albums JOIN artists ON artists.artist_id = albums.artist_id;",
            )
        }
        ui.MenuItem("Tracks per genre") {
            on choose: load_starter(
                d,
                "Tracks per genre",
                "SELECT genres.name, COUNT(*) AS tracks FROM tracks JOIN genres ON genres.genre_id = tracks.genre_id GROUP BY genres.name;",
            )
        }
        ui.MenuItem("Every table of this file") {
            on choose: load_starter(d, "Every table", "SELECT name, type FROM sqlite_master;")
        }
    }
}

/// A result as a grid: one column per field, numbers aligned, a press on a row's mark copying it.
view result_grid(d: Database, o: Outcome, rows: Collection<Row>) {
    if rows.is_empty() {
        ui.EmptyState(
            if not o.columns.is_empty() {
                "The query returned no rows"
            } else if o.is_select { "The query returned no columns" } else { "The statement ran" },
            icon: .table,
            inline: true,
        )
    } else {
        ui.DataTable(name: "db.result.table", selectable: true) {
            columns: {
                for c in o.columns {
                    ui.DataTableColumn(
                        c,
                        kind: if numeric_column(o.numeric, o.columns.index_of(c) ?? 0) {
                            .numeric
                        } else { .prose },
                    )
                }
            }
            for r in ordered_rows(rows, o) {
                ui.DataTableRow(r) {
                    cells: {
                        for c in o.columns {
                            ui.DataTableCell(c, value: cell_at(r, o.columns.index_of(c) ?? 0))
                        }
                    }
                }
            }
            on activate(r: Row): copy_row(d, r)
        }
    }
}

/// Which rows of the table a page holds: its first and last place, and the table's whole count
/// once the catalog counted it.
fn page_range(s: Session) -> Text {
    if s.selected_table == "" {
        return "No table selected"
    }
    if s.page_rows.is_empty() {
        return "No rows"
    }
    let from = s.offset + 1
    let to = s.offset + s.page_rows.len()
    if let t = s.tables.find(x -> x.schema == s.selected_schema and x.name == s.selected_table) {
        if let total = t.row_count {
            return "Rows {from} to {to} of {total}"
        }
    }
    "Rows {from} to {to}"
}

/// One page of the selected table's rows, with the way from page to page.
view data_tab(d: Database, s: Session) {
    if let o = s.pages.first() {
        stack(gap: .related) {
            row(gap: .related, align: .center) {
                ui.Text(page_range(s), role: .caption, secondary: true)
                spacer
                ui.Button("Previous rows", icon: .chevron_left) {
                    on press: previous_page(d)
                }
                ui.Button("Next rows", icon: .chevron_right) {
                    on press: next_page(d)
                }
            }
            if s.selected_table == "" {
                ui.EmptyState(
                    "No table selected",
                    icon: .table,
                    message: "Choose a table to see its rows.",
                )
            } else {
                result_grid(d, o, s.page_rows)
            }
        }
    } else if s.selected_table == "" {
        ui.EmptyState("No table selected", icon: .table, message: "Choose a table to see its rows.")
    } else {
        ui.EmptyState(
            "No rows read yet",
            icon: .table,
            message: "Read the table's rows from the list beside the window.",
        )
    }
}

/// The selected table's columns, keys and indexes.
view structure_tab(s: Session) {
    if s.selected_table == "" {
        ui.EmptyState(
            "No table selected",
            icon: .key,
            message: "Choose a table to see its structure.",
        )
    } else {
        let columns = s.catalog_columns.filter(
            c -> c.schema == s.selected_schema and c.table == s.selected_table,
        )
        stack(gap: .section) {
            text(s.selected_schema + "." + s.selected_table, role: .title)
            ui.Section("Columns") {
                if columns.is_empty() {
                    ui.EmptyState("No columns read yet", icon: .table, inline: true)
                } else {
                    ui.DataTable(name: "db.structure.columns") {
                        columns: {
                            ui.DataTableColumn("Name")
                            ui.DataTableColumn("Type")
                            ui.DataTableColumn("Nullable")
                            ui.DataTableColumn("Key")
                            ui.DataTableColumn("Default")
                        }
                        for c in columns {
                            ui.DataTableRow(c) {
                                cells: {
                                    ui.DataTableCell("Name", value: c.name)
                                    ui.DataTableCell("Type", value: c.decl_type)
                                    ui.DataTableCell(
                                        "Nullable",
                                        value: if c.nullable and not c.primary_key {
                                            "Yes"
                                        } else { "No" },
                                    )
                                    ui.DataTableCell(
                                        "Key",
                                        value: if c.primary_key { "Primary" } else { "" },
                                    )
                                    ui.DataTableCell("Default", value: c.default_value ?? "")
                                }
                            }
                        }
                    }
                }
            }
            if not s.keys.is_empty() {
                ui.Section("Keys") {
                    ui.DataTable(name: "db.structure.keys") {
                        columns: {
                            ui.DataTableColumn("Constraint")
                            ui.DataTableColumn("Column")
                            ui.DataTableColumn("References")
                        }
                        for f in s.keys {
                            ui.DataTableRow(f) {
                                cells: {
                                    ui.DataTableCell("Constraint", value: "Foreign key")
                                    ui.DataTableCell("Column", value: f.column)
                                    ui.DataTableCell(
                                        "References",
                                        value: "{f.ref_table}.{f.ref_column}",
                                    )
                                }
                            }
                        }
                    }
                }
            }
            if not s.indexes.is_empty() {
                ui.Section("Indexes") {
                    ui.DataTable(name: "db.structure.indexes") {
                        columns: {
                            ui.DataTableColumn("Index")
                            ui.DataTableColumn("Unique")
                            ui.DataTableColumn("Columns")
                        }
                        for i in s.indexes {
                            ui.DataTableRow(i) {
                                cells: {
                                    ui.DataTableCell("Index", value: i.name)
                                    ui.DataTableCell(
                                        "Unique",
                                        value: if i.unique { "Yes" } else { "No" },
                                    )
                                    ui.DataTableCell("Columns", value: i.columns)
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The new-connection form: the engine to reach, where it stands, and what signs in.
view connect_dialog(
    d: Database,
    open: State<Bool>,
    local_name: State<Text>,
    local_engine: State<Int>,
    local_host: State<Text>,
    local_port: State<Int>,
    local_database: State<Text>,
    local_user: State<Text>,
    local_file: State<Text>,
    local_password: State<Text>,
) {
    ui.ModalDialog(
        title: "New connection",
        open: open,
        primary: "Add connection",
        secondary: "Cancel",
    ) {
        body: {
            stack(gap: .related) {
                text("Name", role: .caption, secondary: true)
                ui.TextInput(bind: local_name, placeholder: "Connection name", autofocus: true) {
                }
                ui.ToggleGroup(selected: local_engine, label: "Engine") {
                    ui.Segment("SQLite")
                    ui.Segment("PostgreSQL")
                    ui.Segment("MySQL")
                    on change(i): set(local_engine, i)
                        .then(set(local_port, default_port(engine_of(i))))
                }
                if local_engine == 0 {
                    text("SQLite file path", role: .caption, secondary: true)
                    ui.TextInput(bind: local_file, placeholder: "~/Documents/app.sqlite") {
                    }
                } else {
                    text("Host", role: .caption, secondary: true)
                    ui.TextInput(bind: local_host, placeholder: "localhost") {
                    }
                    text("Port", role: .caption, secondary: true)
                    ui.NumberInput(bind: local_port, min: 1, max: 65535)
                    text("Database", role: .caption, secondary: true)
                    ui.TextInput(bind: local_database, placeholder: "database") {
                    }
                    text("User", role: .caption, secondary: true)
                    ui.TextInput(bind: local_user, placeholder: "user") {
                    }
                    text("Password", role: .caption, secondary: true)
                    ui.TextInput(bind: local_password, placeholder: "Password", secret: true) {
                    }
                    text(
                        "The password is kept on this device, never in the shared connection.",
                        role: .caption,
                        secondary: true,
                    )
                }
            }
        }
        on act_primary: add_connection(
                d,
                local_name,
                local_engine,
                local_host,
                local_port,
                local_database,
                local_user,
                local_file,
                local_password,
            )
            .then(set(open, false))
        on dismiss: set(open, false)
    }
}

/// The statement that changes data, waiting for the person to confirm it.
view confirm_dialog(d: Database, s: Session) {
    ui.ModalDialog(
        title: "Run this statement?",
        description: .some(
            if s.name == "" { "No connection is open." } else { "This runs on {s.name}." },
        ),
        open: true,
        primary: "Run statement",
        secondary: "Cancel",
        destructive: true,
    ) {
        body: {
            stack(gap: .related) {
                ui.Text(s.pending, role: .caption)
                text("The database client cannot undo this.", role: .caption, secondary: true)
            }
        }
        on act_primary: confirm_run(d)
        on dismiss: dismiss_run(d)
    }
}
