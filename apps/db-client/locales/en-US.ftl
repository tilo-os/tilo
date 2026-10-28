## Window bar

db-run-query = Run query
db-cancel-query = Cancel query
db-copy-row = Copy row
db-copy-result = Copy result
db-export-csv = Export as CSV
db-view-switcher = View

## Tabs

db-tab-query = Query
db-tab-data = Data
db-tab-structure = Structure
db-tab-query-hint = Write SQL and run it against the open connection.
db-tab-data-hint = The rows of the selected table.
db-tab-structure-hint = The columns, keys and indexes of the selected table.

## Query pane

db-ready = Ready
db-query-no-columns = The query returned no columns.
db-statement-ran = The statement ran.
db-starter-queries = Starter queries
db-sql-placeholder = Write a SQL query
db-sql-editor = SQL

## Results

db-result-rows =
    { $count ->
        [one] 1 row
       *[other] { $count } rows
    } in { $ms } ms
db-result-rows-cut = The first { $count ->
        [one] 1 row
       *[other] { $count } rows
    } in { $ms } ms
db-result-cut-rows = This result was cut at { $limit } rows. Narrow the query, or page through the table instead.
db-result-cut-payload = This result was cut to keep the window responsive: { $limit } is as much of it as is held. Narrow the query to see the rest.
db-result-cut-payload-limit = 8 MB
db-result-affected =
    { $count ->
        [one] 1 row changed
       *[other] { $count } rows changed
    } in { $ms } ms
db-copied-row = Row copied
db-copied-result = Result copied
db-copy-nothing = Select a row to copy first

## Status

db-connected = Connected to { $name }
db-connecting = Connecting to { $name }…
db-running = Running…
db-cancelling = Cancelling the query…
db-cancel-finished = The query has already finished
db-copy-failed = Could not copy to the clipboard
db-cancelled = Query cancelled
db-no-connection = No open connection
db-query-running = A query is already running
db-identity-exhausted = The query counter is exhausted; restart Database Client
db-dismiss-status = Dismiss

## Sidebar

db-navigator = Connections and tables
db-connections = Connections
db-new-connection = New connection
db-remove-connection = Remove connection
db-catalog = Catalog
db-tables = No tables
db-saved-queries = Saved queries

## Data tab

db-no-table = No table selected
db-choose-table-rows = Choose a table to see its rows.
db-choose-table-structure = Choose a table to see its structure.
db-no-connections = No connections yet
db-no-connections-hint = Add a PostgreSQL, MySQL or SQLite database to browse its tables and run queries.
db-no-rows = No rows
db-rows-range = Rows { $from } to { $to } of { $total }
db-next-rows = Next rows
db-previous-rows = Previous rows

## Structure

db-column-name = Name
db-column-type = Type
db-column-nullable = Nullable
db-column-key = Key
db-column-default = Default
db-column-constraint = Constraint
db-column-column = Column
db-column-references = References
db-column-index = Index
db-column-unique = Unique
db-column-columns = Columns
db-keys = Keys
db-indexes = Indexes
db-yes = Yes
db-no = No
db-none = None
db-primary = Primary
db-primary-key = Primary key
db-foreign-key = Foreign key

## Connection dialog

db-new-connection-title = New connection
db-connection-name = Connection name
db-host = Host
db-port = Port
db-database = Database
db-user = User
db-password = Password
db-password-stays = The password stays on this device.
db-file-path = SQLite file path
db-add-connection = Add connection
db-cancel = Cancel
db-untitled = Untitled
db-no-connection-open = No connection is open.
db-cannot-undo = The database client cannot undo this.
db-run-statement-title = Run this statement?
db-run-statement = Run statement

## History: what each change did, by field and by verb

db-client-workspace-sql = Edited the SQL
db-client-workspace-connections = Changed the saved connections
db-client-workspace-saved-queries = Changed the saved queries
db-client-saved-connection-name = Renamed the connection { $item }
db-client-saved-connection-engine = Set the engine of { $item } to { $new ->
    [sqlite] SQLite
    [postgres] PostgreSQL
    [mysql] MySQL
   *[other] { $new }
}
db-client-saved-connection-host = Set the host of { $item } to { $new }
db-client-saved-connection-port = Set the port of { $item } to { $new }
db-client-saved-connection-database = Set the database of { $item } to { $new }
db-client-saved-connection-user = Set the user of { $item } to { $new }
db-client-saved-connection-file-path = Set the file of { $item } to { $new }
db-client-saved-query-name = Renamed the query { $item }
db-client-saved-query-sql = Edited the query { $item }
db-client-add-connection = Added the connection { $name }
db-client-remove-connection = Removed the connection { $item }
db-client-save-query = Saved the query { $name }
db-client-remove-query = Removed the query { $item }

## Running statements, exporting and what a cell shows

db-runs-on = This runs on { $name }.
db-busy-operation = Database operation…
db-export-title = Export query results
db-export-accept = Export CSV
db-export-filter = CSV (comma-separated values)
db-export-choosing = Choose where to save the CSV…
db-exported = { $count ->
    [one] Exported 1 row to { $path }
   *[other] Exported { $count } rows to { $path }
}
db-export-failed = The CSV could not be written: { $error }
db-export-dialog-failed = The save dialog could not open: { $error }
db-export-dialog-closed = The save dialog closed before a place was chosen.
db-cell-truncated = { $value }… [truncated]
db-cell-binary = Binary, { $bytes } bytes
db-unknown-engine = There is no engine called { $engine }.
db-one-statement = One statement runs at a time. { $error }

## Search: the catalogue this app contributes to system search

db-search-kind = Database
db-search-connected-database = Connected database
db-search-active-database = Active database
db-search-schema-in = Schema in { $database }
db-search-column-in = { $type } in { $schema }.{ $table }
db-search-current-result = Current query result
db-search-transaction = Transaction
db-search-query-history = Local query history
db-search-query-entry = Query { $number }: { $summary }
db-search-local-only = Local only. Never shared with collaborators.
db-search-saved-query = Collaborative saved query
