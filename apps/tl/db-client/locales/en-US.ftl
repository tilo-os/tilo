# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-db-client = Database Client

d-so-tilo-db-client-keywords = database, databases, sql, sqlite, postgres, postgresql, mysql, query, queries, tables, table, datenbank, datenbanken, abfrage, abfragen, tabellen

d-opqwnp74 = Connection
    .doc = One saved connection: where it goes and what it signs in with.
    .noun = { $count ->
        [one] connection
       *[other] connections
    }

d-52sa65q7 = Name

d-cg25mito = Engine

d-lsni3mrq = Host

d-axk7b6ff = Port

d-b3gtxaxj = Database

d-3zatdgh2 = User

d-zt2vj4bb = File path
    .doc = The path of a SQLite file on this device; empty for the network engines.

d-7wyfzimv = Used
    .doc = When it was last opened, so the list says which one a person reaches for.

d-opqwnp74-filter-engine = Engine

d-opqwnp74-filter-port = Port

d-opqwnp74-filter-used = Used

d-ytvvlx2s = Saved query
    .doc = A statement a member saved to run again.
    .noun = { $count ->
        [one] saved query
       *[other] saved queries
    }

d-fxmhtvbz = Name

d-rnkrjcin = Sql

d-272ksfhb = Database
    .doc = The workspace's databases: the connections every member sees, the queries they saved, and the statement the editor holds.
    .noun = { $count ->
        [one] database
       *[other] databases
    }

d-djghd3ar = Connections

d-dbkiebu3 = Queries

d-r77o5x22 = Sql
    .doc = The SQL the editor holds, shared so two people work one statement together.

d-nvtukp2b = Sessions
    .doc = Each member's own window on this device's databases.

d-lkucdgg4 = Table info
    .doc = One table or view of an open connection's catalog.
    .noun = { $count ->
        [one] table info
       *[other] table infos
    }

d-yofk7dul = Schema

d-h5zenygt = Name

d-tbo6xpjg = Kind
    .doc = `table` or `view`.

d-7xr4zpm5 = Row count
    .doc = The rows a `COUNT(*)` found; nothing for a view.

d-lkucdgg4-filter-row_count = Row count

d-txlcru2y = Column info
    .noun = { $count ->
        [one] column info
       *[other] column infos
    }

d-qmrcvjjn = Name

d-ffkug4qg = Decl type

d-ifa6r5by = Nullable

d-sew352lt = Primary key

d-4ycs3rch = Default value

d-txlcru2y-filter-nullable = Nullable

d-txlcru2y-filter-primary_key = Primary key

d-fhn2o62o = Key info
    .noun = { $count ->
        [one] key info
       *[other] key infos
    }

d-4sqibo4s = Column

d-hmchr5lj = Ref table

d-nse42ud4 = Ref column

d-fpsvoesg = Index info
    .noun = { $count ->
        [one] index info
       *[other] index infos
    }

d-bmcdtfyt = Name

d-co7n2x5l = Unique

d-ws3nxyhc = Columns

d-fpsvoesg-filter-unique = Unique

d-e57w5mgi = Answer
    .doc = What a statement answered: its columns, every row's text, which cells hold nothing and which columns align as numbers.
    .noun = { $count ->
        [one] answer
       *[other] answers
    }

d-n7guubug = Columns

d-zmjmvjeu = Rows

d-ztnqa552 = Nulls

d-gysqh6ps = Numeric

d-h24jm6lc = Rows affected

d-ydn3x76v = Is select

d-7vfwgci5 = Elapsed ms

d-qf7pdvx7 = Truncated
    .doc = `rows` or `payload` when collection stopped short, empty when the whole result is here.

d-e57w5mgi-filter-rows_affected = Rows affected

d-e57w5mgi-filter-is_select = Is select

d-e57w5mgi-filter-elapsed_ms = Elapsed ms

d-gmleowyz = Started
    .doc = A job the device started for this app.
    .noun = { $count ->
        [one] started
       *[other] starteds
    }

d-ms2izpdh = Job

d-dvf3rvyn = Progress
    .doc = How far a job has got, as the device answers it.
    .noun = { $count ->
        [one] progress
       *[other] progresses
    }

d-dday2ali = State
    .doc = `running`, `done`, `failed`, `cancelled`, or `unknown` for a job this device does not run.

d-bpgwkoxz = Result

d-gacluwxm = Error

d-42j5lhrh = Opened
    .doc = What a connection answered when it opened: the handle this device's engine gives it.
    .noun = { $count ->
        [one] opened
       *[other] openeds
    }

d-yheverkf = Connection

d-42j5lhrh-filter-connection = Connection

d-j7lrdytm = Secret
    .doc = A password this window keeps for a connection.
    .noun = { $count ->
        [one] secret
       *[other] secrets
    }

d-psin3vp7 = Connection

d-3twvmxwg = Password

d-4vk72asw = Run
    .doc = A statement this window ran, one press away again.
    .noun = { $count ->
        [one] run
       *[other] runs
    }

d-6abruthm = Connection

d-nxawaqfc = Sql

d-3fza7fgn = Summary

d-wdgtgr6r = Row
    .doc = One row of a result: its cells as the grid shows them, and which of them hold nothing.
    .noun = { $count ->
        [one] row
       *[other] rows
    }

d-hxhdvye5 = Cells

d-gc6jygal = Nulls

d-j7kxfnjc = Outcome
    .doc = What a statement or a table page answered, with the words the window shows of it.
    .noun = { $count ->
        [one] outcome
       *[other] outcomes
    }

d-qxwjsiht = Seq

d-rym2yft2 = Columns

d-m6wzfydf = Numeric

d-2k7ob6vx = Affected
    .doc = Rows a statement changed; -1 for one that returns rows.

d-kbnzmtdp = Is select

d-4ponyk7v = Elapsed ms

d-iaudq64k = Truncated

d-qzyl3fzm = Summary
    .doc = The words the window says of it.

d-fywguxl3 = Order
    .doc = The column the person ordered it by; empty for the statement's own order.

d-e5veu4zz = Descending

d-j7kxfnjc-filter-seq = Seq

d-j7kxfnjc-filter-affected = Affected

d-j7kxfnjc-filter-is_select = Is select

d-j7kxfnjc-filter-elapsed_ms = Elapsed ms

d-j7kxfnjc-filter-descending = Descending

d-brwj3gzi = Session
    .doc = One person's window on this device's databases: the connection it has open, what the catalog holds, which view it shows and what its last statements answered.
    .noun = { $count ->
        [one] session
       *[other] sessions
    }

d-elhp2adh = Open
    .doc = The saved connection this device opened, by its id; empty while none is.

d-73sz5sht = Name

d-m2eenfte = Connected

d-l6ey6bmu = Handle
    .doc = The handle this device's engine gives the open connection.

d-utbjkevx = Tab

d-62jlzlao = Schemas
    .doc = The schema names the open connection's catalog holds, and its tables.

d-c5axoen6 = Tables

d-qto4lhnk = Columns
    .doc = The selected table's columns, keys and indexes, read when it is chosen.

d-p5c7ywkh = Keys

d-zb4ttvcb = Indexes

d-y5w3id3d = Selected schema

d-efbptxv4 = Selected table

d-4e7a56ci = Offset
    .doc = The row this window shows first, walked page by page, and how many a page holds.

d-ikep24c2 = Page

d-ymfu3vqr = Status
    .doc = What the window says of the last thing it did; empty while it says nothing.

d-z4bjp5xk = Notice
    .doc = Why the last thing did not work; empty while nothing went wrong.

d-p4wji46q = Busy
    .doc = The job in flight; empty while nothing runs.

d-d2dlobjx = Job kind
    .doc = `open` or `query`, so the job's answer is read as what it is.

d-rxkpfssd = Pending
    .doc = A statement that changes data, waiting for the person to confirm it.

d-dvgcj2vb = Confirming

d-pmf3mqgy = Queries
    .doc = A statement's result and a table's page, each kept in its own record, and their rows.

d-d6drmo4f = Pages

d-qfvcjkgl = Result rows

d-iimyuzxn = Page rows

d-g6cz4ye2 = History
    .doc = What this window ran, so it is one press away again.

d-qcislb6v = Secrets
    .doc = What this window keeps of the connections' passwords.

d-xrk5rswl = Working
    .doc = The words the window says while a job runs.

d-brwj3gzi-filter-connected = Connected

d-brwj3gzi-filter-handle = Handle

d-brwj3gzi-filter-tab = Tab

d-brwj3gzi-filter-offset = Offset

d-brwj3gzi-filter-page = Page

d-brwj3gzi-filter-confirming = Confirming

d-57pbpjie = Open ask
    .noun = { $count ->
        [one] open ask
       *[other] open asks
    }

d-wr4rh4km = Engine

d-7ub5mnfo = Host

d-rr35b5xd = Port

d-dmjf4ady = Database

d-g5wwnnue = User

d-y5ginenz = Password

d-eak32p2q = File path

d-57pbpjie-filter-port = Port

d-wjqewyhd = Query ask
    .noun = { $count ->
        [one] query ask
       *[other] query asks
    }

d-64plkkzv = Connection

d-eeqjqurk = Sql

d-wjqewyhd-filter-connection = Connection

d-ld2iurzt = Browse ask
    .noun = { $count ->
        [one] browse ask
       *[other] browse asks
    }

d-ktmsx6gt = Connection

d-urg4wtul = Schema

d-4j7eh7ld = Table

d-qexpbqva = Limit

d-ooei2svb = Offset

d-ld2iurzt-filter-connection = Connection

d-ld2iurzt-filter-limit = Limit

d-ld2iurzt-filter-offset = Offset

d-yzefqy6n = Table ask
    .noun = { $count ->
        [one] table ask
       *[other] table asks
    }

d-fmbqqhio = Connection

d-2maemw4x = Schema

d-wwrvwb2w = Table

d-yzefqy6n-filter-connection = Connection

d-qbscvf2j = Handle
    .noun = { $count ->
        [one] handle
       *[other] handles
    }

d-qjoudlls = Connection

d-qbscvf2j-filter-connection = Connection

d-bi5ghtxw = Engine
    .doc = The engine a connection targets.

d-cyu4lsqt = A file on this device
    .doc = A file on this device.

d-mmyps25g = A PostgreSQL server
    .doc = A PostgreSQL server.

d-mm56qihu = A MySQL server
    .doc = A MySQL server.

d-e7cxwiwu = Tab
    .doc = Which of the connection's three views the window shows.

d-mj57ytfw = The SQL editor and the result of its last statement
    .doc = The SQL editor and the result of its last statement.

d-nefy2tlx = One page of the selected table's rows
    .doc = One page of the selected table's rows.

d-wfq2ds6s = The selected table's columns, keys and indexes
    .doc = The selected table's columns, keys and indexes.

d-w3yjhc5t = Ensure session
    .doc = This person's window, made by their own first action so it is theirs to read and write.

d-olwjmcmu = Connect
    .doc = Opens a saved connection: this device starts the engine on it and reads its catalog.

d-olwjmcmu-c = C

d-d4ianbnu = Disconnect
    .doc = Closes the open connection on this device.

d-awrfepgs = Add connection
    .doc = Adds a connection to the workspace, keeps its password on this device, and opens it.

d-awrfepgs-name = Name

d-awrfepgs-engine_index = Engine index

d-awrfepgs-host = Host

d-awrfepgs-port = Port

d-awrfepgs-database = Database

d-awrfepgs-user = User

d-awrfepgs-file_path = File path

d-awrfepgs-password = Password

d-tanfgf42 = Remove connection
    .doc = Removes a saved connection: its password goes with it on this device.

d-tanfgf42-c = C

d-dctmqew5 = Forget password
    .doc = Drops what this device kept for `c`.

d-dctmqew5-c = C

d-wdk3itk4 = Remember password
    .doc = Keeps `password` for `c` on this device, or forgets it when it is empty.

d-wdk3itk4-c = C

d-wdk3itk4-password = Password

d-2u672vw7 = Save query
    .doc = Saves the statement in the editor under `name`.
    .w-write-a-statement-before-saving-it = Write a statement before saving it

d-2u672vw7-name = Name

d-cslwiduz = Remove query
    .doc = Removes a saved query.

d-cslwiduz-q = Q

d-4liwuntv = Load query
    .doc = Loads a saved query into the editor, ready to run.

d-4liwuntv-q = Q

d-g4n5l4jt = Load starter
    .doc = Puts a starter query in the editor, saved so the person can name it too.

d-g4n5l4jt-name = Name

d-g4n5l4jt-sql = Sql

d-izq7yvoq = Choose tab
    .doc = Shows the view the segment at `index` names.

d-izq7yvoq-index = Index

d-bnqqidok = Show tab
    .doc = Shows one of the window's three views.

d-bnqqidok-tab = Tab

d-mbv7nbbz = Run
    .doc = Runs the statement in the editor, asking first when it changes data.
    .w-open-a-connection-before-running-sql = Open a connection before running SQL
    .w-a-query-is-already-running = A query is already running

d-qwlkh4r3 = Confirm run
    .doc = Runs the statement the person confirmed.

d-ghj5nk2v = Dismiss run
    .doc = Puts away the statement that was waiting for confirmation.

d-dwesnwpp = Run query
    .doc = Runs a saved query against the open connection.

d-dwesnwpp-q = Q

d-jujclfok = Rerun
    .doc = Runs a statement this window ran before.

d-jujclfok-r = R

d-5v2444du = Start statement
    .doc = Starts `sql` on the open connection as a job, so the window stays live and the statement can be stopped.

d-5v2444du-sql = Sql

d-5zv7ksur = Cancel
    .doc = Stops the statement in flight.

d-v6yadfvj = Browse
    .doc = Opens the table `t`: what it is made of, and its first page of rows.

d-v6yadfvj-t = T

d-2ofu7tby = Clear tables
    .doc = Empties the tables a window holds, so a catalog read again never stands beside the one before.

d-oc3wtsgy = Clear structure
    .doc = Empties the columns, keys and indexes a window holds.

d-alqiwzcs = Load structure
    .doc = Reads the selected table's columns, keys and indexes.

d-ads5bu4r = Load page
    .doc = Loads one page of the selected table's rows.

d-waklogb4 = Clear rows
    .doc = Empties a window's rows, so a fresh result never stands beside the one before it.

d-waklogb4-result = Result

d-7swpnxwj = Next page
    .doc = Shows the next page of the selected table's rows.
    .w-this-is-the-last-page = This is the last page

d-7swpnxwj-at = At

d-fjy2y2qc = Previous page
    .doc = Shows the page before the one shown.
    .w-this-is-the-first-page = This is the first page

d-fjy2y2qc-at = At

d-uc6zvpha = Refresh catalog
    .doc = Reads the open connection's catalog again, so a table made elsewhere shows.

d-eoqjcqjj = Order by
    .doc = Orders the result by the column the person chose, or back to the statement's own order.

d-eoqjcqjj-column = Column

d-jzjcmgne = Copy row
    .doc = Copies the result row at `at` as one line of text.

d-jzjcmgne-row = Row

d-audnsxv2 = Copy result
    .doc = Copies the whole result as tab-separated text.

d-iqqpmdyj = Export as CSV
    .doc = The statement's result as a CSV file, for a person to keep.
    .w-only-a-query-that-returns-rows = Only a query that returns rows can be exported as CSV

d-fx2hkovm = Dismiss
    .doc = Clears what the window last said.

d-d5x2kw5n = Check
    .doc = Asks how far the job in flight has got, and folds its answer in.

d-yhmbqfbq = Failed job
    .doc = The window's job could not be asked about at all: it is over, and the window says why.

d-yhmbqfbq-why = Why

d-wavfygex = Opened
    .doc = Takes a connection's handle in, once the engine opened it, and reads its catalog.

d-wavfygex-answer = Answer

d-77ih35sb = Queried
    .doc = Takes a statement's result in, with the words the window shows of it.

d-77ih35sb-answer = Answer

d-v5uqe3yc = Save query~present

d-esekmuai = Run~present

d-2uzibyq5 = Next page~present

d-xndhrcj2 = Previous page~present

d-6d7yw2e6 = Export csv~present

d-yk2y45ko = Window
    .w-database = Database
    .w-connections = Connections
    .w-new-connection = New connection
    .w-that-did-not-work = That did not work
    .w-dismiss = Dismiss
    .w-no-connections-yet = No connections yet
    .w-add-a-postgresql-mysql-or-sqlite = Add a PostgreSQL, MySQL or SQLite database to browse its tables and run queries.
    .w-view = View
    .w-query = Query
    .w-data = Data
    .w-structure = Structure
    .w-choose-a-connection-to-open = Choose a connection to open
    .w-opening-one-reads-its-tables-and = Opening one reads its tables and signs in with what this device keeps.
    .w-add-connection = Add connection
    .w-cancel = Cancel
    .w-name = Name
    .w-connection-name = Connection name
    .w-engine = Engine
    .w-sqlite = SQLite
    .w-postgresql = PostgreSQL
    .w-mysql = MySQL
    .w-sqlite-file-path = SQLite file path
    .w-documents-app-sqlite = ~/Documents/app.sqlite
    .w-host = Host
    .w-localhost = localhost
    .w-port = Port
    .w-database-2 = database
    .w-user = User
    .w-user-2 = user
    .w-password = Password
    .w-the-password-is-kept-on-this = The password is kept on this device, never in the shared connection.

d-5vali6eo = Navigator
    .w-connections = Connections
    .w-no-tables = No tables
    .w-saved-queries = Saved queries
    .w-run-in-this-window = Run in this window

d-e5r6st7e = Query tab
    .w-running = Running…
    .w-cancel-query = Cancel query
    .w-run-query = Run query
    .w-write-a-sql-query = Write a SQL query
    .w-write-a-statement-and-run-it = Write a statement and run it

d-oovsecgv = Result menu
    .w-result = Result
    .w-copy-result = Copy result
    .w-export-as-csv = Export as CSV…
    .w-save-the-query = Save the query…
    .w-order-by = Order by { $c }
    .w-disconnect = Disconnect
    .w-read-the-catalog-again = Read the catalog again

d-u3iyaqlr = Starter queries
    .w-starter-queries = Starter queries
    .w-top-tracks-by-price = Top tracks by price
    .w-albums-with-artist = Albums with artist
    .w-tracks-per-genre = Tracks per genre
    .w-every-table-of-this-file = Every table of this file

d-4tzbmgf2 = Result grid
    .w-the-statement-ran = The statement ran
    .w-the-query-returned-no-rows = The query returned no rows
    .w-db-result-table = db.result.table

d-vn42juiv = Data tab
    .w-no-table-selected = No table selected
    .w-no-rows = No rows
    .w-rows-to = Rows { $value } to { $value2 }
    .w-previous-rows = Previous rows
    .w-next-rows = Next rows
    .w-choose-a-table-to-see-its = Choose a table to see its rows.
    .w-no-rows-read-yet = No rows read yet
    .w-read-the-table-s-rows-from = Read the table's rows from the list beside the window.

d-5u7lwulx = Structure tab
    .w-no-table-selected = No table selected
    .w-choose-a-table-to-see-its = Choose a table to see its structure.
    .w-columns = Columns
    .w-no-columns-read-yet = No columns read yet
    .w-db-structure-columns = db.structure.columns
    .w-name = Name
    .w-type = Type
    .w-nullable = Nullable
    .w-key = Key
    .w-default = Default
    .w-yes = Yes
    .w-no = No
    .w-primary = Primary
    .w-keys = Keys
    .w-db-structure-keys = db.structure.keys
    .w-constraint = Constraint
    .w-column = Column
    .w-references = References
    .w-foreign-key = Foreign key
    .w-indexes = Indexes
    .w-db-structure-indexes = db.structure.indexes
    .w-index = Index
    .w-unique = Unique

d-txz5zpn7 = Confirm dialog
    .w-run-this-statement = Run this statement?
    .w-no-connection-is-open = No connection is open.
    .w-this-runs-on = This runs on { $name }.
    .w-run-statement = Run statement
    .w-cancel = Cancel
    .w-the-database-client-cannot-undo-this = The database client cannot undo this.
