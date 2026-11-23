app so.tilo.sheets "Sheets" (
    icon: .table,
    category: .office,
    tagline: "Spreadsheets with formulas, shared live",
    keywords: [
        "spreadsheet", "spreadsheets", "excel", "calc", "table", "tables", "csv", "xls",
        "xlsx", "formulas", "numbers", "libreoffice calc", "tabelle", "tabellen",
        "kalkulation", "rechnen", "tabellenkalkulation",
    ],
)

/// The currency a bare money format shows in.
enum Currency {
    usd
    eur
    gbp
    jpy
    chf
    cad
    aud
    cny
    inr
}

/// How a cell writes a date: the language's own, ISO, a US or European order, or spelled out.
enum DateStyle {
    language
    iso
    us
    eu
    long
}

/// How a cell writes a clock time: the language's own, 24-hour or 12-hour.
enum TimeStyle {
    language
    h24
    h12
}

/// What a chart draws: columns side by side, or a line over its points.
enum ChartKind {
    column (icon: .bar_chart)
    line (icon: .activity)
}

/// A workbook: the sheets people work in, what was said about their cells, and the formats its
/// numbers, dates and times are read in.
type Workbook (untitled: "Untitled workbook") {
    /// The workbook's name: what its window and an exported file are called.
    title: Text = ""
    sheets: Collection<Sheet>
    /// What people said about cells, on every sheet.
    comments: Collection<Comment>
    /// The currency a bare money format shows in.
    currency: Currency = .usd
    /// How its dates are written.
    date_style: DateStyle = .language
    /// How its times are written.
    time_style: TimeStyle = .language
    /// How many times its rows, columns or sheets have moved. A write that names a `seen` stamp
    /// is refused when the workbook has moved since that stamp was read, so a reference a caller
    /// wrote against never means another cell.
    structure: Int = 0
}

/// One sheet: its tab, its cells as typed, the formats they show in and the size the person gave
/// its columns and rows.
type Sheet (untitled: "Sheet") {
    name: Text
    /// The file it was imported from, by the address of its content.
    source: Text = ""
    /// The tab's colour, by its place in the palette: none for the theme's own.
    color: Option<Int> = .none
    /// Each cell's input as typed, by its A1 reference: a value, or a formula starting `=`.
    cells: Map<Text>
    /// Each formatted cell's format code, by its A1 reference.
    formats: Map<Text>
    /// The cells shown bold.
    bold: Set<Text>
    /// The width in pixels the person gave each column, by its letter.
    widths: Map<Text>
    /// The height in pixels the person gave each row, by its number.
    heights: Map<Text>
    /// The charts drawn over its cells.
    charts: Collection<Chart>
    /// Its tab is hidden.
    hidden: Bool = false

    search {
        sort name a_z
    }
}

/// A chart over a range of a sheet's cells.
type Chart (untitled: "Chart") {
    title: Text
    kind: ChartKind = .column
    /// The cells it charts, as `A1` or `A1:D9`.
    range: Text = ""
}

/// What someone said about cells, and everything said back: the sheet and cells it is about,
/// whether it is resolved, and the replies under it.
type Comment (untitled: "Comment") {
    /// The sheet it is about, by its name.
    sheet: Text
    /// The cells it is about, as `A1` or `A1:D9`.
    range: Text
    text: RichText
    resolved: Bool = false
    replies: Collection<Reply>
}

/// An answer to a comment.
type Reply {
    text: RichText
}

/// One sheet as the engine reads it: its name, whether its tab is hidden, its cells as typed, the
/// formats they show in and the cells shown bold.
type SheetInput {
    name: Text
    hidden: Bool
    cells: Map<Text>
    formats: Map<Text>
    bold: List<Text>
}

/// A workbook as the engine reads it: the sheets it brings and the currency its bare money format
/// shows in.
type BookInput {
    sheets: List<SheetInput>
    currency: Text
}

/// The records a write into a sheet brings: each by its column headers, where they start, and
/// the header that identifies a record's row.
type RecordsRequest {
    records: List<Map<Text>>
    row: Option<Int>
    columns: Option<List<Text>>
    key: Option<Text>
}

/// The record an update writes: the cells that identify its row, and the cells the row takes.
type UpsertRequest {
    records: List<Map<Text>>
    matching: Map<Text>
    value: Map<Text>
}

/// What a records write leaves: each cell's new input and each cell's format, by A1 reference.
type RecordWrites {
    cells: Map<Text>
    formats: Map<Text>
}

root workbook: Workbook

default view workbook for Workbook
default view sheet_window for Sheet
row view sheet_row for Sheet

/// A workbook starts with one sheet.
initial action start(w: Workbook) {
    w.sheets.insert(Sheet(name: "Sheet1"))
}

// ---------------------------------------------------------------------------------------------
// What the engine reads, as texts a view and an action build the same way.

/// `text` as JSON writes it.
fn quoted(text: Text) -> Text {
    json.encode(text)
}

/// One sheet as the engine reads it: its name, whether its tab is hidden, its cells as typed, the
/// formats they show in and the cells shown bold.
fn sheet_input(s: Sheet) -> SheetInput {
    SheetInput(
        name: s.name,
        hidden: s.hidden,
        cells: s.cells,
        formats: s.formats,
        bold: s.bold.map(b -> b),
    )
}

/// A workbook as the engine reads it, out of the sheets the caller wrote.
fn book_of(sheets: List<SheetInput>, currency: Text) -> Text {
    json.encode(BookInput(sheets: sheets, currency: currency))
}

/// The rows a JSON answer holds, or none when it refused.
fn rows_of(answer: Result<Text, Text>) -> List<List<List<Text>>> {
    match answer {
        .ok(text) => json.decode<List<List<List<Text>>>>(text) ?? [],
        .err(_) => [],
    }
}

/// The texts a JSON answer holds, or none when it refused.
fn texts_of(answer: Result<Text, Text>) -> List<Text> {
    match answer {
        .ok(text) => json.decode<List<Text>>(text) ?? [],
        .err(_) => [],
    }
}

/// The text at `i` of `row`, or nothing when it is shorter.
fn text_at(row: List<Text>, i: Int) -> Text {
    row.at(i) ?? ""
}

/// The row at `i` of `rows`, or none when it is shorter.
fn row_at(rows: List<List<Text>>, i: Int) -> List<Text> {
    rows.at(i) ?? []
}

/// What each cell of a workbook shows, sheet by sheet.
fn values_of(book: Text) -> List<List<List<Text>>> {
    rows_of(book.sheet_values())
}

/// The one sheet `name` names in what the engine answered.
fn shown_sheet(shown: List<List<List<Text>>>, name: Text) -> List<List<Text>> {
    shown.find(r -> text_at(row_at(r, 0), 1) == name) ?? []
}

/// The cells of a sheet the engine answered: each its reference, what it shows and how it reads.
fn shown_cells(rows: List<List<Text>>) -> List<List<Text>> {
    rows.filter(r -> text_at(r, 0) == "cell")
}

/// How many columns the sheet's window shows.
fn shown_columns(rows: List<List<Text>>) -> Int {
    text_at(row_at(rows, 0), 2).to_int() ?? 1
}

/// How many rows the sheet's window shows.
fn shown_rows(rows: List<List<Text>>) -> Int {
    text_at(row_at(rows, 0), 3).to_int() ?? 1
}

/// How a cell of `cells` reads, by its reference: `text`, `number`, `bool` or `error`.
fn kind_at(cells: List<List<Text>>, reference: Text) -> Text {
    let found = cells.find(c -> text_at(c, 1) == reference)
    text_at(found ?? [], 3)
}

/// What the cells of `range` of the sheet named `sheet` show, row by row from its first cell to
/// its last, or their input as typed when `formulas`: what a person reads in the grid, for an
/// agent that never opened the window.
action read_cells(w: Workbook, sheet: Text, range: Text = "A1:B2", formulas: Bool = false) -> List<Text> (
    icon: .hash,
    label: "Read cells",
) {
    let found = w.sheets.find(x -> x.name == sheet)
    if let s = found {
        let book = book_of([sheet_input(s)], currency_code(w.currency))
        let cells = shown_cells(shown_sheet(values_of(book), s.name))
        let first_column = column_index(column_of(range_start(range)))
        let last_column = column_index(column_of(range_end(range)))
        let first_row = row_of(range_start(range))
        let last_row = row_of(range_end(range))
        let rows = count_up(first_row, last_row)
        [rows.map(r -> count_up(first_column, last_column).map(c -> cell_text(s, cells, "{column_letter(c)}{r}", formulas)))].flat_map(row -> row)
    } else {
        []
    }
}

/// The text a cell of `cells` reads for `reference`: its input when `formulas`, else what it
/// shows.
fn cell_text(s: Sheet, cells: List<List<Text>>, reference: Text, formulas: Bool) -> Text {
    if formulas {
        input_at(s, reference)
    } else {
        text_at(cell_row(cells, reference), 2)
    }
}

// ---------------------------------------------------------------------------------------------
// References, formats and settings, as code writes them.

/// The ISO code of a currency.
fn currency_code(c: Currency) -> Text {
    match c {
        .usd => "USD"
        .eur => "EUR"
        .gbp => "GBP"
        .jpy => "JPY"
        .chf => "CHF"
        .cad => "CAD"
        .aud => "AUD"
        .cny => "CNY"
        .inr => "INR"
    }
}

/// The code a date style is stored and read by.
fn date_code(s: DateStyle) -> Text {
    match s {
        .language => "Language"
        .iso => "ISO"
        .us => "US"
        .eu => "EU"
        .long => "Long"
    }
}

/// The code a time style is stored and read by.
fn time_code(s: TimeStyle) -> Text {
    match s {
        .language => "Language"
        .h24 => "H24"
        .h12 => "H12"
    }
}

/// The format money is written in, in the workbook's own currency.
fn currency_format(w: Workbook) -> Text {
    "currency:{currency_code(w.currency)}"
}

/// The format a date is written in.
fn date_format(w: Workbook) -> Text {
    "date:{date_code(w.date_style)}"
}

/// The format a time is written in.
fn time_format(w: Workbook) -> Text {
    "time:{time_code(w.time_style)}"
}

/// How many letters an A1 reference opens with: where its row number starts.
fn letters_len(reference: Text) -> Int {
    let up = reference.trim().upper()
    let found = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"]
        .map(d -> up.find(d) ?? up.len())
    found.sort_by(i -> i).first() ?? up.len()
}

/// The column of an A1 reference, by its letters.
fn column_of(reference: Text) -> Text {
    let up = reference.trim().upper()
    up.slice(0, letters_len(up))
}

/// The row of an A1 reference, counting from 1; 0 when it names no row.
fn row_of(reference: Text) -> Int {
    let up = reference.trim().upper()
    up.slice(letters_len(up), up.len()).to_int() ?? 0
}

/// The first cell of an A1 range.
fn range_start(range: Text) -> Text {
    range.split(":").first() ?? ""
}

/// The last cell of an A1 range: the range itself when it names one cell.
fn range_end(range: Text) -> Text {
    let parts = range.split(":")
    if parts.len() > 1 { parts.last() ?? range } else { range }
}

/// Whether another sheet of the workbook is already called `name`, the case of its letters aside.
fn taken_name(w: Workbook, name: Text) -> Bool {
    w.sheets.any(x -> x.name.lower() == name.trim().lower())
}

/// The next sheet name nothing uses yet: `Sheet2` when `Sheet1` is taken.
fn free_name(w: Workbook, n: Int) -> Text {
    if taken_name(w, "Sheet{n}") { free_name(w, n + 1) } else { "Sheet{n}" }
}

/// The input a cell holds, by its reference.
fn input_at(s: Sheet, reference: Text) -> Text {
    s.cells.get(reference) ?? ""
}

// ---------------------------------------------------------------------------------------------
// The sheets and their cells.

/// Adds a sheet at the end, named `name` or the next free `SheetN`.
primary action new_sheet(w: Workbook, name: Text = "") -> Sheet (icon: .plus) {
    let wanted = if name.trim() == "" { free_name(w, w.sheets.len() + 1) } else { name.trim() }
    require not taken_name(w, wanted) "There is already a sheet called {wanted}"
    w.sheets.insert(Sheet(name: wanted))
}

/// Adds a sheet right after `after`, so a new tab stands where the person is.
action new_sheet_after(w: Workbook, after: Sheet, name: Text = "") -> Sheet (icon: .plus) {
    let wanted = if name.trim() == "" { free_name(w, w.sheets.len() + 1) } else { name.trim() }
    require not taken_name(w, wanted) "There is already a sheet called {wanted}"
    let made = w.sheets.insert(Sheet(name: wanted))
    w.sheets.move(made, after: after)
    made
}

/// Names the workbook, so it is found and listed by that name.
action set_title(w: Workbook, title: Text) (icon: .pencil, label: "Name the workbook") {
    require title.trim() != "" "Name the workbook something"
    w.title = title.trim()
}

/// Renames a sheet; every formula that reads it follows it to its new name.
action rename_sheet(w: Workbook, s: Sheet, name: Text) (icon: .pencil, label: "Rename sheet") {
    let wanted = name.trim()
    require wanted != "" "Name the sheet something"
    require not w.sheets.any(x -> x != s and x.name.lower() == wanted.lower()) "There is already a sheet called {wanted}"
    apply_edit(w, "\{\"op\": \"rename\", \"sheet\": \{quoted(s.name)}, \"name\": \{quoted(wanted)}}")
}

/// Copies a sheet and everything on it, under a name of its own, right after it.
action duplicate_sheet(w: Workbook, s: Sheet) (icon: .copy, label: "Duplicate sheet") -> Sheet {
    let made = w.sheets.insert(Sheet(name: free_name(w, w.sheets.len() + 1), source: s.source))
    for k in s.cells.keys() {
        made.cells.put(k, s.cells.get(k) ?? "")
    }
    for k in s.formats.keys() {
        made.formats.put(k, s.formats.get(k) ?? "")
    }
    for b in s.bold.map(x -> x) {
        made.bold.add(b)
    }
    for k in s.widths.keys() {
        made.widths.put(k, s.widths.get(k) ?? "")
    }
    for k in s.heights.keys() {
        made.heights.put(k, s.heights.get(k) ?? "")
    }
    made.hidden = s.hidden
    made.color = s.color
    w.sheets.move(made, after: s)
    made
}

/// Deletes a sheet and what was said about it.
destructive action delete_sheet(w: Workbook, s: Sheet) (icon: .trash, label: "Delete sheet") {
    require w.sheets.len() > 1 "A workbook keeps at least one sheet"
    for c in w.comments.filter(x -> x.sheet == s.name) {
        w.comments.remove(c)
    }
    w.sheets.remove(s)
    w.structure = w.structure + 1
}

/// Puts a sheet's tab right before another, so the person reorders the tabs.
action place_sheet(w: Workbook, s: Sheet, before: Sheet) (label: "Move the sheet") {
    w.sheets.move(s, before: before)
}

/// Moves a sheet's tab to `at`, the place among the tabs a person sees that a drag reports.
action move_sheet(w: Workbook, s: Sheet, at: Int) (label: "Move the sheet") {
    w.sheets.move(s, to: at, among: w.sheets.filter(x -> not x.hidden or x == s))
}

/// Hides a sheet's tab, or shows it again.
action set_sheet_hidden(s: Sheet, hidden: Bool) (label: "Hide the sheet", icon: .eye_off) {
    s.hidden = hidden
}

/// Gives a sheet's tab a colour from the palette, or none for the theme's own.
action set_sheet_color(s: Sheet, color: Option<Int> = .none) (icon: .palette, label: "Tab colour") {
    s.color = color
}

/// Writes the inputs `inputs` into the cells `refs`, by position: an empty input clears its cell.
/// A typed entry keeps the format it is read in (a date, money, a percent), unless the cell has
/// one the person set. A `seen` stamp refuses the write when rows, columns or sheets have moved
/// since it was read.
action write_cells(w: Workbook, sheet: Text, refs: List<Text>, inputs: List<Text>, seen: Text = "") (
    icon: .pencil,
    label: "Write cells",
    did: "wrote cells",
) {
    require seen == "" or seen == w.structure.to_text() "Rows, columns or sheets moved since this write was made from; read the sheet again and write against what it answers now"
    require w.sheets.any(x -> x.name == sheet) "No sheet is called {sheet}"
    if let s = w.sheets.find(x -> x.name == sheet) {
        for i in refs.map((r, i) -> i) {
            let reference = refs.at(i) ?? ""
            let input = inputs.at(i) ?? ""
            if reference != "" {
                if input == "" {
                    if s.cells.get(reference) != .none {
                        s.cells.remove(reference)
                    }
                } else {
                    let entry = entry_of(input, currency_code(w.currency))
                    s.cells.put(reference, text_at(entry, 0))
                    let format = text_at(entry, 1)
                    if format != "general" and s.formats.get(reference) == .none {
                        s.formats.put(reference, format)
                    }
                }
            }
        }
    }
}

/// The input to store for a typed entry, and the format it is read in.
fn entry_of(input: Text, currency: Text) -> List<Text> {
    match input.sheet_entry(currency) {
        .ok(text) => json.decode<List<Text>>(text) ?? [input, "general"],
        .err(_) => [input, "general"],
    }
}

/// Sets cells from a start cell, row by row: what a table handed to the workbook writes.
action set_cells(w: Workbook, s: Sheet, start: Text, rows: List<List<Text>>, seen: Text = "") (
    icon: .pencil,
    label: "Set cells",
    did: "set cells",
) {
    require seen == "" or seen == w.structure.to_text() "Rows, columns or sheets moved since this write was made from; read the sheet again and write against what it answers now"
    let written = rows_json(rows)
    apply_edit(w, "\{\"op\": \"set-cells\", \"sheet\": \{quoted(s.name)}, \"start\": \{quoted(start)}, \"rows\": \{written}}")
}

/// Writes records into a sheet by column header: each record an object keyed by the sheet's
/// headers, so a value a record leaves out leaves its cell instead of shifting the row after it.
/// `row` is the first data row (the row under the headers when left out), `columns` the headers a
/// sheet that has none starts with, and `key` the header that identifies a record's row rather
/// than writing a new one.
action set_records(
    w: Workbook,
    s: Sheet,
    records: List<Map<Text>>,
    row: Option<Int> = .none,
    columns: Option<List<Text>> = .none,
    key: Option<Text> = .none,
) (icon: .table, label: "Write records", did: "wrote records") {
    require not records.is_empty() "Give at least one record to write"
    let request = json.encode(RecordsRequest(records: records, row: row, columns: columns, key: key))
    match json.encode(sheet_input(s)).sheet_records(request) {
        .ok(text) => {
            match json.decode<RecordWrites>(text) {
                .ok(writes) => {
                    for k in writes.cells.keys() {
                        s.cells.put(k, writes.cells.get(k) ?? "")
                    }
                    for k in writes.formats.keys() {
                        s.formats.put(k, writes.formats.get(k) ?? "")
                    }
                }
                .err(_) => {}
            }
        }
        .err(why) => {
            require false why
        }
    }
}

/// Writes a record into a sheet by the values that identify its row: `matching` names the headers
/// and values that find the row, and `value` the cells it takes. A row nothing matches is made
/// under the headers.
action upsert_item(w: Workbook, s: Sheet, matching: Map<Text>, value: Map<Text>) (
    icon: .pencil,
    label: "Update the record",
    did: "updated a record",
) {
    require not matching.keys().is_empty() "Name the values that identify the record"
    let request = json.encode(UpsertRequest(records: [], matching: matching, value: value))
    match json.encode(sheet_input(s)).sheet_records(request) {
        .ok(text) => {
            match json.decode<RecordWrites>(text) {
                .ok(writes) => {
                    for k in writes.cells.keys() {
                        s.cells.put(k, writes.cells.get(k) ?? "")
                    }
                    for k in writes.formats.keys() {
                        s.formats.put(k, writes.formats.get(k) ?? "")
                    }
                }
                .err(_) => {}
            }
        }
        .err(why) => {
            require false why
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The clipboard: the chosen cells out, and what is on it in.

/// Copies the cells a person chose to their clipboard as a table: a line per row and a tab
/// between columns, which a sheet, a word processor or a mail app reads.
action copy_cells(text: Text) uses clipboard (label: "Copy", key: "Ctrl+C") {
    clipboard.copy(text)
}

/// Cuts the cells a person chose: copies them as `copy_cells` does, then clears them.
action cut_cells(w: Workbook, s: Sheet, area: Text, text: Text) uses clipboard (
    label: "Cut",
    key: "Ctrl+X",
) {
    clipboard.copy(text)
    clear_cells(w, s, area)
}

/// Writes the table on the person's clipboard into the cells from `at`: a line per row and a tab
/// between columns. Nothing on the clipboard writes nothing.
action paste_cells(w: Workbook, s: Sheet, at: Text) uses clipboard (label: "Paste", key: "Ctrl+V") {
    if let text = clipboard.paste() {
        if text.trim() != "" {
            set_cells(w, s, at, text.trim().split("\n").map(line -> line.split("\t")))
        }
    }
}

/// Clears the cells `area` names.
action clear_cells(w: Workbook, s: Sheet, area: Text) (label: "Clear the cells", did: "cleared cells") {
    let refs = rows_of_range(area).flat_map(r -> columns_of_range(area).map(c -> "{c}{r}"))
    write_cells(w, s.name, refs, refs.map(x -> ""))
}

/// Sets the format of the cells `range` (a code such as `currency:EUR`, `percent` or `date:ISO`),
/// and their weight when `bold` is given.
action format_cells(w: Workbook, s: Sheet, range: Text, format: Text = "", bold: Option<Bool> = .none) (
    icon: .paint_bucket,
    label: "Format cells",
    did: "formatted cells",
) {
    let shaped = if format == "" { "" } else { ", \"format\": \{quoted(format)}" }
    let weighted = if let b = bold { if b { ", \"bold\": true" } else { ", \"bold\": false" } } else { "" }
    apply_edit(w, "\{\"op\": \"format\", \"sheet\": \{quoted(s.name)}, \"range\": \{quoted(range)}\{shaped}\{weighted}}")
}

/// Makes the cells `range` bold, or regular when any one of them already is.
action toggle_bold(w: Workbook, s: Sheet, range: Text) (icon: .bold, label: "Bold", key: "Ctrl+B") {
    let some = s.bold.any(b -> in_range(range, b))
    format_cells(w, s, range, bold: .some(not some))
}

/// Whether a reference falls inside a range.
fn in_range(range: Text, reference: Text) -> Bool {
    let column = column_index(column_of(reference))
    let row = row_of(reference)
    column >= column_index(column_of(range_start(range)))
        and column <= column_index(column_of(range_end(range)))
        and row >= row_of(range_start(range))
        and row <= row_of(range_end(range))
}

/// Writes the sum of the numbers above `at` into it, the run stopping at a blank, a word or an
/// existing total: the AutoSum button.
action autosum(w: Workbook, s: Sheet, at: Text) (icon: .plus, label: "AutoSum") {
    let book = book_of([sheet_input(s)], currency_code(w.currency))
    let cells = shown_cells(shown_sheet(values_of(book), s.name))
    let column = column_of(at)
    let row = row_of(at)
    require row > 1 "Nothing above to sum"
    let run = numeric_run(s, cells, column, row - 1)
    require run > 0 "No numbers above to sum"
    s.cells.put(at, "=SUM({column}{row - run}:{column}{row - 1})")
}

/// How many number cells stand in a run ending at `row` of `column`.
fn numeric_run(s: Sheet, cells: List<List<Text>>, column: Text, row: Int) -> Int {
    if row < 1 {
        0
    } else {
        let raw = input_at(s, "{column}{row}").trim()
        if raw == "" {
            0
        } else if raw.upper().contains("SUM(") {
            0
        } else if raw.starts_with("=") or kind_at(cells, "{column}{row}") == "number" {
            1 + numeric_run(s, cells, column, row - 1)
        } else {
            0
        }
    }
}

/// Adds up each column of the selection into the row under it.
action sum_selection(w: Workbook, s: Sheet, range: Text) (icon: .plus, label: "Sum the selection") {
    apply_edit(w, "\{\"op\": \"sum\", \"sheet\": \{quoted(s.name)}, \"range\": \{quoted(range)}}")
}

// ---------------------------------------------------------------------------------------------
// Columns, by their letters and their places.

/// The place of a column, counting from 1: `A` is 1 and `AA` is 27.
fn column_index(letters: Text) -> Int {
    let up = letters.trim().upper()
    let n = up.len()
    if n == 1 {
        letter_value(up)
    } else if n == 2 {
        letter_value(up.slice(0, 1)) * 26 + letter_value(up.slice(1, 2))
    } else {
        letter_value(up.slice(0, 1)) * 676
            + letter_value(up.slice(1, 2)) * 26
            + letter_value(up.slice(2, 3))
    }
}

/// What one letter of a column's name stands for: `A` is 1.
fn letter_value(letter: Text) -> Int {
    let places = [
        "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M",
        "N", "O", "P", "Q", "R", "S", "T", "U", "V", "W", "X", "Y", "Z",
    ]
    let found = places.index_of(letter.trim().upper()) ?? 0
    found + 1
}

/// The letter of the column at `place`, counting from 1.
fn column_letter(place: Int) -> Text {
    let places = [
        "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M",
        "N", "O", "P", "Q", "R", "S", "T", "U", "V", "W", "X", "Y", "Z",
    ]
    if place <= 26 {
        places.at(place - 1) ?? "A"
    } else if place <= 702 {
        "{places.at((place - 1) / 26 - 1) ?? "A"}{places.at((place - 1) % 26) ?? "A"}"
    } else {
        let high = places.at((place - 1) / 676 - 1) ?? "A"
        let middle = places.at((place - 1) % 676 / 26) ?? "A"
        let low = places.at((place - 1) % 26) ?? "A"
        "{high}{middle}{low}"
    }
}

/// The rows of texts a table writes, as JSON: one array for each row.
fn rows_json(rows: List<List<Text>>) -> Text {
    let each = rows.map(row -> column_json(row))
    "[{each.join(", ")}]"
}

/// One row of texts as JSON.
fn column_json(row: List<Text>) -> Text {
    let values = row.map(v -> quoted(v)).join(", ")
    "[{values}]"
}

// ---------------------------------------------------------------------------------------------
// Rows, columns, sorting and filling: each a change of the sheet the engine applies.

/// Inserts `count` rows above the row `at`, counting from 1.
action insert_rows(w: Workbook, s: Sheet, at: Int, count: Int = 1) (icon: .list_plus, label: "Insert rows") {
    apply_edit(w, "\{\"op\": \"insert-rows\", \"sheet\": \{quoted(s.name)}, \"at\": {at - 1}, \"count\": {count}}")
}

/// Deletes the rows `rows`, counting from 1.
destructive action delete_rows(w: Workbook, s: Sheet, rows: List<Int>) (icon: .list_plus, label: "Delete rows") {
    let places = rows.map(r -> (r - 1).to_text()).join(", ")
    apply_edit(w, "\{\"op\": \"delete-rows\", \"sheet\": \{quoted(s.name)}, \"rows\": [{places}]}")
}

/// Inserts `count` columns left of the column `at`, by its letters.
action insert_columns(w: Workbook, s: Sheet, at: Text, count: Int = 1) (icon: .columns_2, label: "Insert columns") {
    apply_edit(w, "\{\"op\": \"insert-columns\", \"sheet\": \{quoted(s.name)}, \"at\": \{quoted(at.upper())}, \"count\": {count}}")
}

/// Deletes the columns `columns`, by their letters.
destructive action delete_columns(w: Workbook, s: Sheet, columns: List<Text>) (icon: .columns_2, label: "Delete columns") {
    let named = columns.map(c -> quoted(c.upper())).join(", ")
    apply_edit(w, "\{\"op\": \"delete-columns\", \"sheet\": \{quoted(s.name)}, \"columns\": [{named}]}")
}

/// Sorts the rows of `range` by the values under the column `by`, by its letters.
action sort_rows(w: Workbook, s: Sheet, range: Text, by: Text, descending: Bool = false) (icon: .arrow_down_wide_narrow, label: "Sort rows") {
    let down = if descending { "true" } else { "false" }
    apply_edit(w, "\{\"op\": \"sort-rows\", \"sheet\": \{quoted(s.name)}, \"range\": \{quoted(range)}, \"by\": \{quoted(by.upper())}, \"descending\": {down}}")
}

/// Extends the cells `source` over `target`, as dragging the fill handle does: numbers count on,
/// dates follow their step and a pattern of words repeats.
action fill(w: Workbook, s: Sheet, source: Text, target: Text) (icon: .pencil, label: "Fill") {
    apply_edit(w, "\{\"op\": \"fill\", \"sheet\": \{quoted(s.name)}, \"source\": \{quoted(source)}, \"target\": \{quoted(target)}}")
}

/// Applies one structural change to the workbook and keeps every sheet as the engine leaves it:
/// its cells, their formats and the cells shown bold, its name when the change renamed it, and
/// the cells its charts and comments are about.
action apply_edit(w: Workbook, change: Text) (label: "Change the sheet", did: "changed a sheet") {
    let book = book_of(w.sheets.map(x -> sheet_input(x)), currency_code(w.currency))
    let moved = rows_of(book.sheet_edit(change))
    for i in w.sheets.map((x, i) -> i) {
        if let target = w.sheets.at(i) {
            let rows = moved.at(i) ?? []
            let head = row_at(rows, 0)
            let named = text_at(head, 1)
            if head.len() > 2 and named != "" and named != target.name {
                target.name = named
            }
            // The charts and comments keep the cells they are about as those cells move.
            let charts = target.charts.map(c -> c.range)
            let kept = texts_of(change.sheet_ranges(target.name, charts))
            for k in target.charts.map((c, k) -> k) {
                if let chart = target.charts.at(k) {
                    let range = kept.at(k) ?? ""
                    if range == "" {
                        target.charts.remove(chart)
                    } else {
                        chart.range = range
                    }
                }
            }
            let said = w.comments.filter(c -> c.sheet == target.name)
            let about = said.map(c -> c.range)
            let places = texts_of(change.sheet_ranges(target.name, about))
            for k in said.map((c, k) -> k) {
                if let c = said.at(k) {
                    let range = places.at(k) ?? ""
                    if range == "" {
                        w.comments.remove(c)
                    } else {
                        c.range = range
                    }
                }
            }
            // Every cell, as the engine left it.
            for old in target.cells.keys() {
                target.cells.remove(old)
            }
            for old in target.formats.keys() {
                target.formats.remove(old)
            }
            for old in target.bold.map(b -> b) {
                target.bold.remove(old)
            }
            for row in rows {
                let reference = text_at(row, 1)
                if text_at(row, 0) == "cell" {
                    let input = text_at(row, 2)
                    let format = text_at(row, 3)
                    if input != "" {
                        target.cells.put(reference, input)
                    }
                    if format != "" {
                        target.formats.put(reference, format)
                    }
                    if text_at(row, 4) == "1" {
                        target.bold.add(reference)
                    }
                } else if text_at(row, 0) == "sheet" {
                    target.hidden = text_at(row, 2) == "1"
                }
            }
        }
    }
    w.structure = w.structure + 1
}

// ---------------------------------------------------------------------------------------------
// Charts.

/// Charts the cells `range` as a column or a line chart, titled `title`.
action add_chart(s: Sheet, range: Text, kind: ChartKind, title: Text) (icon: .bar_chart, label: "Add chart") -> Chart {
    require range.trim() != "" "Chart a range of cells"
    let named = if title.trim() == "" { kind.label() } else { title.trim() }
    s.charts.insert(Chart(title: named, kind: kind, range: range.trim().upper()))
}

/// Changes a chart: its title, and its kind or the cells it charts when given.
action edit_chart(c: Chart, title: Text = "", kind: Option<ChartKind> = .none, range: Text = "") (icon: .pencil, label: "Edit chart") {
    if title.trim() != "" {
        c.title = title.trim()
    }
    if let k = kind {
        c.kind = k
    }
    if range.trim() != "" {
        c.range = range.trim().upper()
    }
}

/// Deletes a chart.
destructive action delete_chart(s: Sheet, c: Chart) (icon: .trash, label: "Delete chart") {
    s.charts.remove(c)
}

// ---------------------------------------------------------------------------------------------
// Comments.

/// Says `text` about the cells `range` of a sheet.
action comment(w: Workbook, sheet: Text, range: Text, text: RichText) (
    icon: .message_square_plus,
    label: "Comment",
    did: "commented",
) -> Comment {
    require not text.is_empty() "Write a comment first"
    require range.trim() != "" "Comment on a cell or a range"
    require w.sheets.any(x -> x.name == sheet) "No sheet is called {sheet}"
    w.comments.insert(Comment(sheet: sheet, range: range.trim().upper(), text: text))
}

/// Answers a comment.
action reply(c: Comment, text: RichText) (icon: .reply, label: "Reply", did: "replied") -> Reply {
    require not text.is_empty() "Write a reply first"
    c.replies.insert(Reply(text: text))
}

/// Resolves a comment, or opens it again.
action resolve(c: Comment, resolved: Bool = not c.resolved) (
    label: if resolved { "Resolve" } else { "Reopen" },
    icon: .circle_check,
) {
    c.resolved = resolved
}

/// Changes what a comment says. Only its author may.
action edit_comment(c: Comment, text: RichText = c.text) (icon: .pencil, label: "Edit comment") {
    require created(c)?.who == invoker "Only its author can edit a comment"
    require not text.is_empty() "Write a comment first"
    c.text = text
}

/// Changes what a reply says. Only its author may.
action edit_reply(r: Reply, text: RichText = r.text) (icon: .pencil, label: "Edit reply") {
    require created(r)?.who == invoker "Only its author can edit a reply"
    require not text.is_empty() "Write a reply first"
    r.text = text
}

/// Deletes a comment and its replies. Only its author may.
destructive action delete_comment(w: Workbook, c: Comment) (icon: .trash, label: "Delete comment") {
    require created(c)?.who == invoker "Only its author can delete a comment"
    w.comments.remove(c)
}

// ---------------------------------------------------------------------------------------------
// Settings.

/// Sets the workbook's currency, date style and time style: what a cell's bare money, date and
/// time formats are read in.
action set_settings(
    w: Workbook,
    currency: Option<Currency> = .none,
    date_style: Option<DateStyle> = .none,
    time_style: Option<TimeStyle> = .none,
) (icon: .settings_2, label: "Document settings") {
    if let c = currency {
        w.currency = c
    }
    if let d = date_style {
        w.date_style = d
    }
    if let t = time_style {
        w.time_style = t
    }
}

// ---------------------------------------------------------------------------------------------
// Files: a table, a workbook or a Notion export in, and the workbook or one sheet out.

/// Imports a table, a workbook or a Notion export as new sheets after the ones the workbook holds.
action import_table(w: Workbook, file: File) (
    icon: .file_plus,
    label: "Import a table",
    did: "imported a table",
    opens: [
        "text/csv",
        "text/tab-separated-values",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "application/vnd.oasis.opendocument.spreadsheet",
        "application/zip",
        .csv,
        .tsv,
        .xlsx,
        .ods,
    ],
) {
    let imported = rows_of(file.sheets())
    require not imported.is_empty() "This file holds no table to import"
    for rows in imported {
        let head = row_at(rows, 0)
        let named = text_at(head, 1)
        let name = if named == "" or taken_name(w, named) {
            free_name(w, w.sheets.len() + 1)
        } else {
            named
        }
        let made = w.sheets.insert(Sheet(name: name, source: file.url))
        made.hidden = text_at(head, 2) == "1"
        for row in rows {
            if text_at(row, 0) == "cell" {
                let reference = text_at(row, 1)
                let input = text_at(row, 2)
                let format = text_at(row, 3)
                if input != "" {
                    made.cells.put(reference, input)
                }
                if format != "" {
                    made.formats.put(reference, format)
                }
                if text_at(row, 4) == "1" {
                    made.bold.add(reference)
                }
            }
        }
    }
    if w.title == "" {
        w.title = named_after(file.name)
    }
}

/// The name a file gives the workbook: its own, without its ending.
fn named_after(name: Text) -> Text {
    let parts = name.split(".")
    if parts.len() > 1 {
        name.slice(0, name.len() - (parts.last() ?? "").len() - 1)
    } else {
        name
    }
}

/// What an exported file is called: the workbook's name, or `Untitled`, with its ending.
fn export_name(w: Workbook, ending: Text) -> Text {
    let stem = if w.title.trim() == "" { "Untitled" } else { w.title.trim() }
    "{stem}.{ending}"
}

/// What a spreadsheet file holds, as the sheets the app keeps: each sheet's name, whether its tab
/// is hidden, and its cells as typed by their A1 reference. An agent reads a table without the
/// window.
action read_file(file: File) -> Text (icon: .hash, label: "Read a table file") {
    match file.sheets() {
        .ok(sheets) => sheets,
        .err(why) => {
            require false why
            ""
        }
    }
}

/// The workbook as the engine reads it, as text: its sheets, its cells as typed and the currency
/// its bare money format shows in. An agent reads a sheet's content without the window.
action read_content(w: Workbook) -> Text (icon: .hash, label: "Read the workbook") {
    book_of(w.sheets.map(x -> sheet_input(x)), currency_code(w.currency))
}

/// The workbook as an Excel file.
action export_workbook(w: Workbook) (icon: .download, label: "Export the workbook") -> File {
    let book = book_of(w.sheets.map(x -> sheet_input(x)), currency_code(w.currency))
    File.of_sheets(export_name(w, "xlsx"), book, "xlsx")
}

/// The workbook as an OpenDocument spreadsheet.
action export_ods(w: Workbook) (icon: .download, label: "Export as OpenDocument") -> File {
    let book = book_of(w.sheets.map(x -> sheet_input(x)), currency_code(w.currency))
    File.of_sheets(export_name(w, "ods"), book, "ods")
}

/// One sheet as a table file.
action export_table(w: Workbook, s: Sheet) (icon: .download, label: "Export this sheet") -> File {
    let book = book_of([sheet_input(s)], currency_code(w.currency))
    File.of_sheets(export_name(w, "csv"), book, "csv")
}
