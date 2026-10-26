## Window bar

db-run-query = Run query
db-cancel-query = Cancel query
db-copy-row = Copy row
db-copy-result = Copy result
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
