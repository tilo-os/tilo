# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-sheets = Sheets

d-so-tilo-sheets-keywords = spreadsheet, spreadsheets, excel, calc, table, tables, csv, xls, xlsx, formulas, numbers, libreoffice calc, tabelle, tabellen, kalkulation, rechnen, tabellenkalkulation

d-lpy3iqko = Workbook
    .doc = A workbook: the sheets people work in, what was said about their cells, and the formats its numbers, dates and times are read in.
    .noun = { $count ->
        [one] workbook
       *[other] workbooks
    }
    .untitled = Untitled workbook

d-meovofcz = Title
    .doc = The workbook's name: what its window and an exported file are called.

d-pkbcb37f = Sheets

d-xwh3fsa2 = Comments
    .doc = What people said about cells, on every sheet.

d-r7ipin2u = Currency
    .doc = The currency a bare money format shows in.

d-33jfkdmv = Date style
    .doc = How its dates are written.

d-wvlwcqju = Time style
    .doc = How its times are written.

d-qetlxegv = Structure
    .doc = How many times its rows, columns or sheets have moved.

d-lpy3iqko-filter-currency = Currency

d-lpy3iqko-filter-date_style = Date style

d-lpy3iqko-filter-time_style = Time style

d-lpy3iqko-filter-structure = Structure

d-hvuojqql = Sheet
    .doc = One sheet: its tab, its cells as typed, the formats they show in and the size the person gave its columns and rows.
    .noun = { $count ->
        [one] sheet
       *[other] sheets
    }
    .untitled = Sheet

d-l36xpi4o = Name

d-hotvvuda = Source
    .doc = The file it was imported from, by the address of its content.

d-6jss3eyn = Color
    .doc = The tab's colour, by its place in the palette: none for the theme's own.

d-6xebiw3n = Cells
    .doc = Each cell's input as typed, by its A1 reference: a value, or a formula starting `=`.

d-s2rtlgut = Formats
    .doc = Each formatted cell's format code, by its A1 reference.

d-vy3vanj2 = Bold
    .doc = The cells shown bold.

d-d2szrgfm = Widths
    .doc = The width in pixels the person gave each column, by its letter.

d-km7cpljo = Heights
    .doc = The height in pixels the person gave each row, by its number.

d-lsltfxgz = Charts
    .doc = The charts drawn over its cells.

d-5lw5xfqk = Hidden
    .doc = Its tab is hidden.

d-5t7lxzhn = Chart
    .doc = A chart over a range of a sheet's cells.
    .noun = { $count ->
        [one] chart
       *[other] charts
    }
    .untitled = Chart

d-iipmzvk6 = Title

d-todwcigp = Kind

d-urs2nvtf = Range
    .doc = The cells it charts, as `A1` or `A1:D9`.

d-5t7lxzhn-filter-kind = Kind

d-phviv2im = Comment
    .doc = What someone said about cells, and everything said back: the sheet and cells it is about, whether it is resolved, and the replies under it.
    .noun = { $count ->
        [one] comment
       *[other] comments
    }
    .untitled = Comment

d-fhfswroi = Sheet
    .doc = The sheet it is about, by its name.

d-lfxsmldw = Range
    .doc = The cells it is about, as `A1` or `A1:D9`.

d-437c6ryq = Text

d-nosjmdwd = Resolved

d-64zcrzt6 = Replies

d-phviv2im-filter-resolved = Resolved

d-uepmzroh = Reply
    .doc = An answer to a comment.
    .noun = { $count ->
        [one] reply
       *[other] replies
    }

d-rehztzuy = Text

d-knf5kn3p = Sheet input
    .doc = One sheet as the engine reads it: its name, whether its tab is hidden, its cells as typed, the formats they show in and the cells shown bold.
    .noun = { $count ->
        [one] sheet input
       *[other] sheet inputs
    }

d-d4n4c4j6 = Name

d-iitrsrcd = Hidden

d-owwnyfq7 = Cells

d-ftagyrff = Formats

d-xrjhhdys = Bold

d-knf5kn3p-filter-hidden = Hidden

d-jjadtjsi = Book input
    .doc = A workbook as the engine reads it: the sheets it brings and the currency its bare money format shows in.
    .noun = { $count ->
        [one] book input
       *[other] book inputs
    }

d-z2cfbg2g = Sheets

d-ktfpf6rx = Currency

d-mtwd2vmw = Records request
    .doc = The records a write into a sheet brings: each by its column headers, where they start, and the header that identifies a record's row.
    .noun = { $count ->
        [one] records request
       *[other] records requests
    }

d-quawedop = Records

d-uslgkxn3 = Row

d-vstmdg7x = Columns

d-bou7m624 = Key

d-mtwd2vmw-filter-row = Row

d-gfuv5blj = Upsert request
    .doc = The record an update writes: the cells that identify its row, and the cells the row takes.
    .noun = { $count ->
        [one] upsert request
       *[other] upsert requests
    }

d-styf7w4a = Records

d-s3tgj4q3 = Matching

d-wlypu6mi = Value

d-xeeacqkk = Record writes
    .doc = What a records write leaves: each cell's new input and each cell's format, by A1 reference.
    .noun = { $count ->
        [one] record write
       *[other] record writes
    }

d-tloxk2t5 = Cells

d-chuotih4 = Formats

d-buhbm4qy = Extent
    .doc = How many columns and rows a sheet's grid holds.
    .noun = { $count ->
        [one] extent
       *[other] extents
    }

d-5pdxnqaj = Columns

d-hmh5rxxy = Rows

d-buhbm4qy-filter-columns = Columns

d-buhbm4qy-filter-rows = Rows

d-v4pp7ji7 = Currency
    .doc = The currency a bare money format shows in.

d-zxu4qyy3 = Usd

d-huupsnlo = Eur

d-igylpfzc = Gbp

d-xhdezkkc = Jpy

d-ggtoebqt = Chf

d-5yqwjgio = Cad

d-ssbeykcj = Aud

d-s26t7qen = Cny

d-j5anmeo2 = Inr

d-kqxuafem = Date style
    .doc = How a cell writes a date: the language's own, ISO, a US or European order, or spelled out.

d-btsyslxm = Language

d-54bvpzxs = Iso

d-34m3b4vu = Us

d-brjun66q = Eu

d-se4rytie = Long

d-fg2wvrxg = Time style
    .doc = How a cell writes a clock time: the language's own, 24-hour or 12-hour.

d-lrx52kof = Language

d-wixfnyqk = H24

d-tdy2qln2 = H12

d-opthjwt4 = Chart kind
    .doc = What a chart draws: columns side by side, or a line over its points.

d-uoghytvo = Column

d-h54dd7ap = Line

d-svssol4q = Start
    .doc = A workbook starts with one sheet.

d-wcbfzgpm = Read cells
    .doc = What the cells of `range` of the sheet named `sheet` show, row by row from its first cell to its last, or their input as typed when `formulas`: what a person reads in the grid, for an agent that never opened the window.

d-wcbfzgpm-sheet = Sheet

d-wcbfzgpm-range = Range

d-wcbfzgpm-formulas = Formulas

d-62tp4euu = New sheet
    .doc = Adds a sheet at the end, named `name` or the next free `SheetN`.
    .w-there-is-already-a-sheet-called = There is already a sheet called { $trim }

d-62tp4euu-name = Name

d-gwcwwwqa = New sheet after
    .doc = Adds a sheet right after `after`, so a new tab stands where the person is.
    .w-there-is-already-a-sheet-called = There is already a sheet called { $trim }

d-gwcwwwqa-after = After

d-gwcwwwqa-name = Name

d-dszdasx2 = Name the workbook
    .doc = Names the workbook, so it is found and listed by that name.
    .w-name-the-workbook-something = Name the workbook something

d-dszdasx2-title = Title

d-7ivjtkdd = Rename sheet
    .doc = Renames a sheet; every formula that reads it follows it to its new name.
    .w-name-the-sheet-something = Name the sheet something
    .w-there-is-already-a-sheet-called = There is already a sheet called { $trim }

d-7ivjtkdd-s = S

d-7ivjtkdd-name = Name

d-cdihtodp = Duplicate sheet
    .doc = Copies a sheet and everything on it, under a name of its own, right after it.

d-cdihtodp-s = S

d-p4k23van = Delete sheet
    .doc = Deletes a sheet and what was said about it.
    .w-a-workbook-keeps-at-least-one = A workbook keeps at least one sheet

d-p4k23van-s = S

d-w4cv5do2 = Move the sheet
    .doc = Puts a sheet's tab right before another, so the person reorders the tabs.

d-w4cv5do2-s = S

d-w4cv5do2-before = Before

d-unj46rx2 = Move the sheet
    .doc = Moves a sheet's tab to `at`, the place among the tabs a person sees that a drag reports.

d-unj46rx2-s = S

d-unj46rx2-at = At

d-fjqaau4y = Hide the sheet
    .doc = Hides a sheet's tab, or shows it again.

d-fjqaau4y-hidden = Hidden

d-euxzo6lx = Tab colour
    .doc = Gives a sheet's tab a colour from the palette, by its place in it.

d-euxzo6lx-color = Color

d-2gzyc2ec = No tab colour
    .doc = Gives a sheet's tab no colour of its own, so it takes the theme's.

d-rpgrbjdp = Write cells
    .doc = Writes the inputs `inputs` into the cells `refs`, by position: an empty input clears its cell.
    .did = wrote cells
    .w-rows-columns-or-sheets-moved-since = Rows, columns or sheets moved since this write was made from; read the sheet again and write against what it answers now
    .w-no-sheet-is-called = No sheet is called { $sheet }

d-rpgrbjdp-sheet = Sheet

d-rpgrbjdp-refs = Refs

d-rpgrbjdp-inputs = Inputs

d-rpgrbjdp-seen = Seen

d-f3rx5gta = Set cells
    .doc = Sets cells from a start cell, row by row: what a table handed to the workbook writes.
    .did = set cells
    .w-rows-columns-or-sheets-moved-since = Rows, columns or sheets moved since this write was made from; read the sheet again and write against what it answers now

d-f3rx5gta-s = S

d-f3rx5gta-start = Start

d-f3rx5gta-rows = Rows

d-f3rx5gta-seen = Seen

d-4kfnyiqo = Write records
    .doc = Writes records into a sheet by column header: each record an object keyed by the sheet's headers, so a value a record leaves out leaves its cell instead of shifting the row after it.
    .did = wrote records
    .w-give-at-least-one-record-to = Give at least one record to write

d-4kfnyiqo-s = S

d-4kfnyiqo-records = Records

d-4kfnyiqo-row = Row

d-4kfnyiqo-columns = Columns

d-4kfnyiqo-key = Key

d-d5sanour = Update the record
    .doc = Writes a record into a sheet by the values that identify its row: `matching` names the headers and values that find the row, and `value` the cells it takes.
    .did = updated a record
    .w-name-the-values-that-identify-the = Name the values that identify the record

d-d5sanour-s = S

d-d5sanour-matching = Matching

d-d5sanour-value = Value

d-zupeeccj = Copy
    .doc = Copies the cells a person chose to their clipboard as a table: a line per row and a tab between columns, which a sheet, a word processor or a mail app reads.

d-zupeeccj-text = Text

d-wjhzbhf5 = Cut
    .doc = Cuts the cells a person chose: copies them as `copy_cells` does, then clears them.

d-wjhzbhf5-s = S

d-wjhzbhf5-area = Area

d-wjhzbhf5-text = Text

d-b3ma6s46 = Paste
    .doc = Writes the table on the person's clipboard into the cells from `at`: a line per row and a tab between columns.

d-b3ma6s46-s = S

d-b3ma6s46-at = At

d-ftfb6qyj = Clear the cells
    .doc = Clears the cells `area` names.
    .did = cleared cells

d-ftfb6qyj-s = S

d-ftfb6qyj-area = Area

d-2nxs4pim = Format cells
    .doc = Sets the format of the cells `range` (a code such as `currency:EUR`, `percent` or `date:ISO`), and their weight when `bold` is given.
    .did = formatted cells

d-2nxs4pim-s = S

d-2nxs4pim-range = Range

d-2nxs4pim-format = Format

d-2nxs4pim-bold = Bold

d-v3owv47c = Bold
    .doc = Makes the cells `range` bold, or regular when any one of them already is.

d-v3owv47c-s = S

d-v3owv47c-range = Range

d-4dirhtsc = AutoSum
    .doc = Writes the sum of the numbers above `at` into it, the run stopping at a blank, a word or an existing total: the AutoSum button.
    .w-nothing-above-to-sum = Nothing above to sum
    .w-no-numbers-above-to-sum = No numbers above to sum

d-4dirhtsc-s = S

d-4dirhtsc-at = At

d-gfwmfq3n = Sum the selection
    .doc = Adds up each column of the selection into the row under it.

d-gfwmfq3n-s = S

d-gfwmfq3n-range = Range

d-jtdk43lt = Insert rows
    .doc = Inserts `count` rows above the row `at`, counting from 1.

d-jtdk43lt-s = S

d-jtdk43lt-at = At

d-jtdk43lt-count = Count

d-3lmozqcl = Delete rows
    .doc = Deletes the rows `rows`, counting from 1.

d-3lmozqcl-s = S

d-3lmozqcl-rows = Rows

d-l75woqft = Insert columns
    .doc = Inserts `count` columns left of the column `at`, by its letters.

d-l75woqft-s = S

d-l75woqft-at = At

d-l75woqft-count = Count

d-tx5jrq6p = Delete columns
    .doc = Deletes the columns `columns`, by their letters.

d-tx5jrq6p-s = S

d-tx5jrq6p-columns = Columns

d-p2rgpwyj = Sort rows
    .doc = Sorts the rows of `range` by the values under the column `by`, by its letters.

d-p2rgpwyj-s = S

d-p2rgpwyj-range = Range

d-p2rgpwyj-by = By

d-p2rgpwyj-descending = Descending

d-4kddm3qk = Fill
    .doc = Extends the cells `source` over `target`, as dragging the fill handle does: numbers count on, dates follow their step and a pattern of words repeats.

d-4kddm3qk-s = S

d-4kddm3qk-pair = Pair

d-7bar2fy5 = Fill down
    .doc = Fills the rows under the first row of `range` from it: the first row copied down, counting on as a dragged fill handle does.
    .w-there-is-nothing-below-the-first = There is nothing below the first row to fill

d-7bar2fy5-s = S

d-7bar2fy5-range = Range

d-s7hn4a22 = Fill right
    .doc = Fills the columns right of the first column of `range` from it: the first column copied across, counting on as a dragged fill handle does.
    .w-there-is-nothing-right-of-the = There is nothing right of the first column to fill

d-s7hn4a22-s = S

d-s7hn4a22-range = Range

d-fvh4tlq7 = Change the sheet
    .doc = Applies one structural change to the workbook and keeps every sheet as the engine leaves it: its cells, their formats and the cells shown bold, its name when the change renamed it, and the cells its charts and comments are about.
    .did = changed a sheet

d-fvh4tlq7-change = Change

d-u4dwmiae = Add chart
    .doc = Charts the cells `range` as a column or a line chart, titled `title`.
    .w-chart-a-range-of-cells = Chart a range of cells

d-u4dwmiae-range = Range

d-u4dwmiae-kind = Kind

d-u4dwmiae-title = Title

d-f4hypxhq = Edit chart
    .doc = Changes a chart: its title, and its kind or the cells it charts when given.

d-f4hypxhq-title = Title

d-f4hypxhq-kind = Kind

d-f4hypxhq-range = Range

d-ln6xq4ku = Delete chart
    .doc = Deletes a chart.

d-ln6xq4ku-c = C

d-vcafbmpe = Comment
    .doc = Says `text` about the cells `range` of a sheet.
    .did = commented
    .w-write-a-comment-first = Write a comment first
    .w-comment-on-a-cell-or-a = Comment on a cell or a range
    .w-no-sheet-is-called = No sheet is called { $sheet }

d-vcafbmpe-sheet = Sheet

d-vcafbmpe-range = Range

d-vcafbmpe-text = Text

d-rz22a6sp = Reply
    .doc = Answers a comment.
    .did = replied
    .w-write-a-reply-first = Write a reply first

d-rz22a6sp-text = Text

d-xa7mhkpn = Resolve
    .doc = Resolves a comment, or opens it again.
    .w-resolve = Resolve
    .w-reopen = Reopen

d-xa7mhkpn-resolved = Resolved

d-uz7t2i7r = Edit comment
    .doc = Changes what a comment says.
    .w-only-its-author-can-edit-a = Only its author can edit a comment
    .w-write-a-comment-first = Write a comment first

d-uz7t2i7r-text = Text

d-uophjddh = Edit reply
    .doc = Changes what a reply says.
    .w-only-its-author-can-edit-a = Only its author can edit a reply
    .w-write-a-reply-first = Write a reply first

d-uophjddh-text = Text

d-xnqm5thi = Delete reply
    .doc = Deletes a reply.
    .w-only-its-author-can-delete-a = Only its author can delete a reply

d-xnqm5thi-r = R

d-jk2p2gfj = Delete comment
    .doc = Deletes a comment and its replies.
    .w-only-its-author-can-delete-a = Only its author can delete a comment

d-jk2p2gfj-c = C

d-qw6kcwed = Document settings
    .doc = Sets the workbook's currency, date style and time style: what a cell's bare money, date and time formats are read in.

d-qw6kcwed-currency = Currency

d-qw6kcwed-date_style = Date style

d-qw6kcwed-time_style = Time style

d-f3ht7636 = Import a table
    .doc = Imports a table, a workbook or a Notion export as new sheets after the ones the workbook holds.
    .did = imported a table
    .w-this-file-holds-no-table-to = This file holds no table to import

d-f3ht7636-file = File

d-gvcfeuyz = Read a table file
    .doc = What a spreadsheet file holds, as the sheets the app keeps: each sheet's name, whether its tab is hidden, and its cells as typed by their A1 reference.

d-gvcfeuyz-file = File

d-5dvgmfem = Read the workbook
    .doc = The workbook as the engine reads it, as text: its sheets, its cells as typed and the currency its bare money format shows in.

d-3nmp3zj7 = Export the workbook
    .doc = The workbook as an Excel file.

d-pxduk66t = Export as OpenDocument
    .doc = The workbook as an OpenDocument spreadsheet.

d-l4ukyyfc = Export this sheet
    .doc = One sheet as a table file.

d-l4ukyyfc-s = S

d-g6kghyzj = Edit cells
    .doc = Writes the cells a grid edit reports: pairs of reference and input, in order.
    .did = edited cells

d-g6kghyzj-s = S

d-g6kghyzj-pairs = Pairs

d-5atwoosm = Resize the column
    .doc = Keeps the width the person dragged a column to: the pair carries its letter and its width.
    .did = resized a column

d-5atwoosm-pair = Pair

d-v4gotdqj = Resize the row
    .doc = Keeps the height the person dragged a row to: the pair carries its number and its height.
    .did = resized a row

d-v4gotdqj-pair = Pair

d-7qeu4kom = New sheet~present

d-kwdpptzn = New sheet after~present

d-nkotdgkz = Set title~present

d-z7iqjltu = Rename sheet~present

d-5mbpzlvl = Delete sheet~present

d-gmjtiz5j = Write cells~present

d-z3vgzzkq = Set cells~present

d-7pgvi7c4 = Set records~present

d-2j6pbu2h = Upsert item~present

d-2clifysf = Autosum~present

d-wuzrpfk3 = Fill down~present

d-d3esk7xt = Fill right~present

d-r6i4ot7t = Add chart~present

d-xx7my2go = Comment~present

d-tq6llztc = Reply~present

d-imasocpj = Resolve~present

d-4ei5hwub = Edit comment~present

d-45othlx6 = Edit reply~present

d-u5d6cfi4 = Delete reply~present

d-d3azxjha = Delete comment~present

d-vodfdztg = Import table~present

d-ibft2idy = Workbook
    .w-comments = Comments
    .w-charts = Charts
    .w-close = Close
    .w-nothing-said-about-this-sheet-yet = Nothing said about this sheet yet
    .w-resolved = Resolved
    .w-edit-the-comment = Edit the comment
    .w-save = Save
    .w-cancel = Cancel
    .w-reply = Reply
    .w-reopen = Reopen
    .w-resolve = Resolve
    .w-delete = Delete
    .w-comment-on = Comment on { $area }
    .w-comment = Comment
    .w-no-chart-over-this-sheet-yet = No chart over this sheet yet
    .w-nothing-in-to-chart = Nothing in { $range } to chart
    .w-chart-title = Chart title
    .w-cells-as-a1-d9 = Cells, as A1:D9
    .w-show-the-cells = Show the cells
    .w-more = More
    .w-rename-or-move-the-chart = Rename or move the chart…
    .w-chart-the-chosen-cells = Chart the chosen cells
    .w-column-chart = Column chart
    .w-line-chart = Line chart
    .w-delete-the-chart = Delete the chart
    .w-sheets = Sheets
    .w-add-sheet = Add sheet
    .w-sheet = Sheet
    .w-rename = Rename…
    .w-duplicate = Duplicate
    .w-tab-colour = Tab colour…
    .w-show-the-tab = Show the tab
    .w-hide-the-tab = Hide the tab
    .w-move-before = Move before { $name }
    .w-delete-the-sheet = Delete the sheet
    .w-rename-the-sheet = Rename the sheet
    .w-rename-2 = Rename
    .w-tab-colour-2 = Tab colour
    .w-done = Done
    .w-no-colour = No colour
    .w-document-settings = Document settings
    .w-how-its-numbers-dates-and-times = How its numbers, dates and times are written
    .w-money = Money: { $currency_name }
    .w-dates = Dates: { $date_name }
    .w-times = Times: { $time_name }
    .w-this-workbook-has-no-sheet = This workbook has no sheet

d-stzolo66 = Sheet window
    .w-comments = Comments
    .w-charts = Charts
    .w-close = Close
    .w-nothing-said-about-this-sheet-yet = Nothing said about this sheet yet
    .w-resolved = Resolved
    .w-edit-the-comment = Edit the comment
    .w-save = Save
    .w-cancel = Cancel
    .w-reply = Reply
    .w-reopen = Reopen
    .w-resolve = Resolve
    .w-delete = Delete
    .w-comment-on = Comment on { $area }
    .w-comment = Comment
    .w-no-chart-over-this-sheet-yet = No chart over this sheet yet
    .w-nothing-in-to-chart = Nothing in { $range } to chart
    .w-chart-title = Chart title
    .w-cells-as-a1-d9 = Cells, as A1:D9
    .w-show-the-cells = Show the cells
    .w-more = More
    .w-rename-or-move-the-chart = Rename or move the chart…
    .w-chart-the-chosen-cells = Chart the chosen cells
    .w-column-chart = Column chart
    .w-line-chart = Line chart
    .w-delete-the-chart = Delete the chart
    .w-sheets = Sheets
    .w-add-sheet = Add sheet
    .w-sheet = Sheet
    .w-rename = Rename…
    .w-duplicate = Duplicate
    .w-tab-colour = Tab colour…
    .w-show-the-tab = Show the tab
    .w-hide-the-tab = Hide the tab
    .w-move-before = Move before { $name }
    .w-delete-the-sheet = Delete the sheet
    .w-rename-the-sheet = Rename the sheet
    .w-rename-2 = Rename
    .w-tab-colour-2 = Tab colour
    .w-done = Done
    .w-no-colour = No colour
    .w-document-settings = Document settings
    .w-how-its-numbers-dates-and-times = How its numbers, dates and times are written
    .w-money = Money: { $currency_name }
    .w-dates = Dates: { $date_name }
    .w-times = Times: { $time_name }
    .w-this-workbook-has-no-sheet = This workbook has no sheet

d-b7vdig4c = Sheet row
    .w-cells = { $len } cells

d-msl2xtvn = Reply row
    .w-delete-reply = Delete reply
