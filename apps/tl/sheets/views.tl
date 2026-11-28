/// The workbook's window: the grid of the sheet the person has open, the bar over it holding the
/// name box and the formula bar, the tabs under it, and the panel beside it with what was said
/// about the cells and the charts drawn over them.
view workbook(w: Workbook) {
    state open_sheet: Option<Sheet> = w.sheets.first()
    state area: Text = "A1"
    state typed: Text = ""
    state formulas: Bool = false
    state panel: Text = ""
    state jumping: Bool = false
    workbook_page(w, open_sheet, area, typed, formulas, panel, jumping)
}

/// A sheet's window of its own: the workbook it is in, with that sheet open.
view sheet_window(s: Sheet) {
    state open_sheet: Option<Sheet> = .some(s)
    state area: Text = "A1"
    state typed: Text = ""
    state formulas: Bool = false
    state panel: Text = ""
    state jumping: Bool = false
    workbook_page(root Workbook, open_sheet, area, typed, formulas, panel, jumping)
}

/// One sheet as a row: its name and how many cells it holds.
view sheet_row(s: Sheet) {
    ui.ListRow(title: s.name, secondary: "{s.cells.keys().len()} cells")
}

/// The workbook's page: its bar, its grid and its tabs.
template view workbook_page(
    w: Workbook,
    open_sheet: State<Option<Sheet>>,
    area: State<Text>,
    typed: State<Text>,
    formulas: State<Bool>,
    panel: State<Text>,
    jumping: State<Bool>,
) {
    state renaming: Bool = false
    state rename_text: Text = ""
    state coloring: Bool = false
    state settings_open: Bool = false
    let shown = open_sheet ?? w.sheets.first()
    if let s = shown {
        // The workbook as the engine reads it, and what each of its cells shows.
        let book = book_of(w.sheets.map(x -> sheet_input(x)), currency_code(w.currency))
        let rows = shown_sheet(values_of(book), s.name)
        let cells = shown_cells(rows)
        std.page(w, canvas: true, fill: true) {
            header: {
                workbook_bar(w, s, area, typed, formulas, panel, jumping, settings_open)
            }
            ui.Docked {
                content: {
                    row(fill) {
                        sheet_grid(w, s, area, typed, cells, formulas)
                        if panel != "" {
                            side_panel(w, s, area, panel, book)
                        }
                    }
                }
                foot: {
                    workbook_tabs(w, s, open_sheet, area, renaming, rename_text, coloring)
                }
            }
        }
        workbook_dialogs(w, s, renaming, rename_text, coloring, settings_open)
    } else {
        ui.EmptyState("This workbook has no sheet", icon: .table) {
            on act: new_sheet(w)
        }
    }
}

/// The bar over the grid: the File and Format menus, the name box, the formula bar and the
/// controls beside them.
template view workbook_bar(
    w: Workbook,
    s: Sheet,
    area: State<Text>,
    typed: State<Text>,
    formulas: State<Bool>,
    panel: State<Text>,
    jumping: State<Bool>,
    settings_open: State<Bool>,
) {
    file_menu(w, s, settings_open)
    format_menu(w, s, area, jumping)
    // The name box, and the entry the window's Ctrl+G opens to type a reference into from
    // anywhere: it takes the keyboard as it stands, and closes as the reference is given.
    if jumping {
        ui.TextInput(bind: area, placeholder: "A1", entry: true, autofocus: true) {
            on submit(reference): set(area, reference.trim().upper()).then(set(jumping, false))
        }
    } else {
        ui.TextInput(bind: area, placeholder: "A1") {
            on submit(reference): set(area, reference.trim().upper())
        }
    }
    ui.TextInput(bind: typed, placeholder: "Value or formula") {
        on submit: write_cells(w, s.name, [range_start(area)], [typed]).then(set(typed, ""))
    }
    spacer
    ui.Button("Bold", icon: .bold, icon_only: true, toggled: s.bold.any(b -> in_range(area, b))) {
        on press: toggle_bold(w, s, area)
    }
    ui.Button("AutoSum", icon: .plus, icon_only: true) {
        on press: autosum(w, s, range_start(area))
    }
    ui.Button(
        if formulas { "Show results" } else { "Show formulas" },
        icon: if formulas { .eye } else { .code },
        icon_only: true,
        toggled: formulas,
    ) {
        on press: set(formulas, not formulas)
    }
    ui.Button("Comments and charts", icon: .panel_right, icon_only: true, toggled: panel != "") {
        on press: if panel == "" { set(panel, "comments") } else { set(panel, "") }
    }
}

/// The workbook's own commands: a table in and out, and the document's settings.
template view file_menu(w: Workbook, s: Sheet, settings_open: State<Bool>) {
    ui.MenuButton("File", icon: .file_plus) {
        ui.MenuItem("Import a table…", icon: .file_plus) {
            on choose: file.choose(
                import_table(w),
                accept: [
                    "text/csv",
                    "text/tab-separated-values",
                    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                    "application/vnd.oasis.opendocument.spreadsheet",
                    "application/zip",
                ],
            )
        }
        ui.MenuItem("Export the workbook…", icon: .download) {
            on choose: file.save(export_workbook(w), name: export_name(w, "xlsx"))
        }
        ui.MenuItem("Export as OpenDocument…", icon: .download) {
            on choose: file.save(export_ods(w), name: export_name(w, "ods"))
        }
        ui.MenuItem("Export this sheet as a table…", icon: .download) {
            on choose: file.save(export_table(w, s), name: export_name(w, "csv"))
        }
        ui.MenuSeparator
        ui.MenuItem("Document settings…", icon: .settings_2) {
            on choose: set(settings_open, true)
        }
        ui.MenuSeparator
        ui.MenuItem("Duplicate this sheet", icon: .copy) {
            on choose: duplicate_sheet(w, s)
        }
        ui.MenuItem("Delete this sheet", icon: .trash, destructive: true) {
            on choose: delete_sheet(w, s)
        }
    }
}

/// The formats a cell shows its value in, and the ways to change the cells themselves.
template view format_menu(w: Workbook, s: Sheet, area: State<Text>, jumping: State<Bool>) {
    ui.MenuButton("Format", icon: .paint_bucket) {
        ui.MenuItem("Go to a cell…", icon: .corner_down_left, key: "Ctrl+G") {
            on choose: set(jumping, true)
        }
        ui.MenuSeparator
        ui.MenuItem("Number", icon: .hash) {
            on choose: format_cells(w, s, area, format: "number")
        }
        ui.MenuItem("Money", icon: .wallet) {
            on choose: format_cells(w, s, area, format: currency_format(w))
        }
        ui.MenuItem("Percent", icon: .slash) {
            on choose: format_cells(w, s, area, format: "percent")
        }
        ui.MenuItem("Date", icon: .calendar) {
            on choose: format_cells(w, s, area, format: date_format(w))
        }
        ui.MenuItem("Time", icon: .clock) {
            on choose: format_cells(w, s, area, format: time_format(w))
        }
        ui.MenuItem("Text", icon: .type) {
            on choose: format_cells(w, s, area, format: "text")
        }
        ui.MenuItem("Yes / no", icon: .check) {
            on choose: format_cells(w, s, area, format: "boolean")
        }
        ui.MenuItem("General", icon: .eraser) {
            on choose: format_cells(w, s, area, format: "general")
        }
        ui.MenuSeparator
        ui.MenuItem("Sum this selection", icon: .plus) {
            on choose: sum_selection(w, s, area)
        }
        ui.MenuItem("Fill down", icon: .arrow_down) {
            on choose: fill_down(w, s, area)
        }
        ui.MenuItem("Fill right", icon: .arrow_right) {
            on choose: fill_right(w, s, area)
        }
        ui.MenuSeparator
        ui.MenuItem("Insert rows above", icon: .list_plus) {
            on choose: insert_rows(w, s, row_of(range_start(area)))
        }
        ui.MenuItem("Delete these rows", icon: .trash, destructive: true) {
            on choose: delete_rows(w, s, rows_of_range(area))
        }
        ui.MenuItem("Insert columns left", icon: .columns_2) {
            on choose: insert_columns(w, s, column_of(range_start(area)))
        }
        ui.MenuItem("Delete these columns", icon: .trash, destructive: true) {
            on choose: delete_columns(w, s, columns_of_range(area))
        }
        ui.MenuSeparator
        ui.MenuItem("Sort the rows ascending", icon: .arrow_down_wide_narrow) {
            on choose: sort_rows(w, s, area, column_of(range_start(area)))
        }
        ui.MenuItem("Sort the rows descending", icon: .arrow_up) {
            on choose: sort_rows(w, s, area, column_of(range_start(area)), descending: true)
        }
    }
}

/// The rows a range covers, counting from 1.
fn rows_of_range(range: Text) -> List<Int> {
    let first = row_of(range_start(range))
    let last = row_of(range_end(range))
    if first > last { [] } else { count_up(first, last) }
}

/// The numbers from `from` to `to`, both counted in.
fn count_up(from: Int, to: Int) -> List<Int> {
    if from > to {
        []
    } else {
        let rest = count_up(from + 1, to)
        [[from], rest].flat_map(x -> x)
    }
}

/// The columns a range covers, by their letters.
fn columns_of_range(range: Text) -> List<Text> {
    let first = column_index(column_of(range_start(range)))
    let last = column_index(column_of(range_end(range)))
    if first > last {
        []
    } else {
        count_up(first, last).map(i -> column_letter(i))
    }
}

/// Where the person's selection is in the sheet, in its own units: the first column and row
/// they chose, counting from 1, and how many columns and rows it covers. What presence says.
fn area_spot(range: Text) -> List<Float> {
    let left = column_index(column_of(range_start(range)))
    let right = column_index(column_of(range_end(range)))
    let top = row_of(range_start(range))
    let bottom = row_of(range_end(range))
    let first = if left <= right { left } else { right }
    let last = if left <= right { right } else { left }
    let high = if top <= bottom { top } else { bottom }
    let low = if top <= bottom { bottom } else { top }
    // A1 is the first cell there is: a range that named none stands there rather than at a
    // cell no sheet has.
    let first = first.max(1)
    let high = high.max(1)
    let last = last.max(first)
    let low = low.max(high)
    [first.to_float(), high.to_float(), (last - first + 1).to_float(), (low - high + 1).to_float()]
}

/// Other members at work in the workbook, as presence reports them: the cells each chose, as
/// the grid draws them.
fn sheet_peers(w: Workbook) -> List<ui.SheetPeer> {
    spots(w).map(p -> .peer(area: peer_area(p.sx, p.sy, p.wide, p.tall), who: p.who))
}

/// The cells a member's place covers, as `A1` or `A1:B2`.
fn peer_area(x: Float, y: Float, wide: Float, tall: Float) -> Text {
    let first = cell_reference(x.to_int(), y.to_int())
    if wide <= 0.0 and tall <= 0.0 {
        first
    } else {
        "{first}:{cell_reference(x.to_int() + wide.to_int() - 1, y.to_int() + tall.to_int() - 1)}"
    }
}

/// One cell, counting its column and row from 1.
fn cell_reference(column: Int, row: Int) -> Text {
    "{column_letter(column)}{row}"
}

/// The grid of the sheet the person has open.
template view sheet_grid(
    w: Workbook,
    s: Sheet,
    area: State<Text>,
    typed: State<Text>,
    cells: List<List<Text>>,
    formulas: State<Bool>,
) {
    let count = shown_count()
    // Where the person works in the workbook, as presence: the cells they chose, in the sheet's
    // own units. The platform says it to the workspace and draws everyone else's from it.
    let at = area_spot(area)
    let first = at.at(0) ?? 0.0
    let top = at.at(1) ?? 0.0
    let across = at.at(2) ?? 1.0
    let down = at.at(3) ?? 1.0
    presence(w, first, top, first, top, across, down)
    ui.Sheet(
        name: .some(s.name),
        columns: count.columns,
        rows: count.rows,
        column_width: .some(112.0),
        row_height: .some(28.0),
        sizes: sizes_of(s),
        cells: cells_of(s, cells, formulas),
        selection: .some(area),
        peers: sheet_peers(w),
        editable: true,
    ) {
        on select(chosen): set(area, chosen).then(set(typed, input_at(s, range_start(chosen))))
        on edit(pairs): edit_cells(w, s, pairs)
        on copy(text): copy_cells(text)
        on cut(text): cut_cells(w, s, area, text)
        on paste: paste_cells(w, s, range_start(area))
        on resized_column(pair): resize_column(s, pair)
        on resized_row(pair): resize_row(s, pair)
        on fill(pair): fill(w, s, pair)
    }
}

/// How many columns and rows a sheet's grid holds.
type Extent {
    columns: Int
    rows: Int
}

/// How many columns and rows the grid holds: what the sheet reaches, and at least a window's.
fn shown_count() -> Extent {
    Extent(columns: 26, rows: 200)
}

/// The cells a sheet's grid shows: what each holds, and how it reads.
fn cells_of(s: Sheet, cells: List<List<Text>>, formulas: Bool) -> List<ui.SheetCell> {
    cells.map(
        c -> .cell(
            cell: text_at(c, 1),
            text: if formulas { input_at(s, text_at(c, 1)) } else { text_at(c, 2) },
            kind: if formulas { .text } else { kind_of(text_at(c, 3)) },
            bold: s.bold.contains(text_at(c, 1)),
        ),
    )
}

/// How a cell's text reads, as the grid draws it.
fn kind_of(kind: Text) -> ui.SheetCellKind {
    if kind == "number" {
        .number
    } else if kind == "error" { .error } else if kind == "bool" { .bool } else { .text }
}

/// Writes the cells a grid edit reports: pairs of reference and input, in order.
action edit_cells(w: Workbook, s: Sheet, pairs: List<Text>) (
    label: "Edit cells",
    did: "edited cells",
) {
    for i in pairs.map((x, i) -> i) {
        if i % 2 == 0 {
            let reference = pairs.at(i) ?? ""
            let input = pairs.at(i + 1) ?? ""
            if reference != "" {
                if input == "" {
                    if s.cells.get(reference) != .none { s.cells.remove(reference) }
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

/// Keeps the width the person dragged a column to: the pair carries its letter and its width.
action resize_column(s: Sheet, pair: List<Text>) (
    label: "Resize the column",
    did: "resized a column",
) {
    let letter = pair.at(0) ?? ""
    let width = pair.at(1) ?? ""
    if letter != "" and width != "" { s.widths.put(letter, width) }
}

/// Keeps the height the person dragged a row to: the pair carries its number and its height.
action resize_row(s: Sheet, pair: List<Text>) (label: "Resize the row", did: "resized a row") {
    let number = pair.at(0) ?? ""
    let height = pair.at(1) ?? ""
    if number != "" and height != "" { s.heights.put(number, height) }
}

/// The columns and rows the person sized.
fn sizes_of(s: Sheet) -> List<ui.SheetSize> {
    let wide: List<ui.SheetSize> = s.widths
        .keys()
        .map(k -> .column(column: k, width: pixels_of(s.widths.get(k) ?? "", 112)))
    let tall: List<ui.SheetSize> = s.heights
        .keys()
        .map(
            k -> .row(
                row: (k.to_int() ?? 1).to_float(),
                height: pixels_of(s.heights.get(k) ?? "", 28),
            ),
        )
    [wide, tall].flat_map(x -> x)
}

/// The pixels a size holds, or `fallback` when it holds none.
fn pixels_of(text: Text, fallback: Int) -> Float {
    (text.to_int() ?? fallback).to_float()
}

/// The workbook's sheets as tabs, with the way to add one and the open sheet's own menu.
// ---------------------------------------------------------------------------------------------
// The tabs under the grid.
template view workbook_tabs(
    w: Workbook,
    s: Sheet,
    open_sheet: State<Option<Sheet>>,
    area: State<Text>,
    renaming: State<Bool>,
    rename_text: State<Text>,
    coloring: State<Bool>,
) {
    row(gap: .tight, align: .center) {
        // The tabs are a run: a person drags one to the place it should stand.
        ui.DragRun(axis: .horizontal, distribute: .start, name: "Sheets") {
            on drop(moved: Sheet, at: Int): move_sheet(w, moved, at)
            for tab in w.sheets.filter(x -> not x.hidden or x == s) {
                ui.DragItem(tab, title: tab.name) {
                    on activate: set(open_sheet, tab).then(set(area, "A1"))
                    ui.Button(tab.name, toggled: tab == s, size: .small) {
                        on press: set(open_sheet, tab).then(set(area, "A1"))
                    }
                }
            }
        }
        ui.Button("Add sheet", icon: .plus, icon_only: true, size: .small) {
            on press: new_sheet_after(w, s)
        }
        spacer
        ui.MenuButton("Sheet", icon: .more_horizontal, icon_only: true) {
            ui.MenuItem("Rename…", icon: .pencil) {
                on choose: set(rename_text, s.name).then(set(renaming, true))
            }
            ui.MenuItem("Duplicate", icon: .copy) {
                on choose: duplicate_sheet(w, s)
            }
            ui.MenuItem("Tab colour…", icon: .palette) {
                on choose: set(coloring, true)
            }
            ui.MenuItem(if s.hidden { "Show the tab" } else { "Hide the tab" }, icon: .eye_off) {
                on choose: set_sheet_hidden(s, not s.hidden)
            }
            ui.MenuSeparator
            for other in w.sheets.filter(x -> x != s) {
                ui.MenuItem("Move before {other.name}", icon: .arrow_left) {
                    on choose: place_sheet(w, s, other)
                }
            }
            ui.MenuSeparator
            ui.MenuItem("Delete the sheet", icon: .trash, destructive: true) {
                on choose: delete_sheet(w, s)
            }
        }
    }
}

/// The panel beside the grid: the comments about the open sheet, or its charts.
// ---------------------------------------------------------------------------------------------
// What was said about the cells, and the charts over them.
template view side_panel(w: Workbook, s: Sheet, area: State<Text>, panel: State<Text>, book: Text) {
    ui.Card {
        row(gap: .tight, align: .center) {
            ui.Button("Comments", size: .small, toggled: panel == "comments") {
                on press: set(panel, "comments")
            }
            ui.Button("Charts", size: .small, toggled: panel == "charts") {
                on press: set(panel, "charts")
            }
            spacer
            ui.Button("Close", icon: .x, icon_only: true, size: .small) {
                on press: set(panel, "")
            }
        }
        scroll {
            if panel == "comments" {
                comments_panel(w, s, area)
            } else {
                charts_panel(w, s, area, book)
            }
        }
    }
}

/// What was said about the open sheet, and the field to say something about the cells the person
/// chose.
template view comments_panel(w: Workbook, s: Sheet, area: State<Text>) {
    state draft: RichText = RichText("")
    let said = w.comments.filter(c -> c.sheet == s.name)
    if said.is_empty() {
        ui.Text("Nothing said about this sheet yet", secondary: true)
    }
    for c in said {
        comment_card(w, c, area)
    }
    ui.RichInput(bind: draft, placeholder: "Comment on {area}") {
        on submit: comment(w, s.name, area, draft).then(set(draft, RichText("")))
    }
    ui.Button("Comment", icon: .message_square_plus, size: .small) {
        on press: comment(w, s.name, area, draft).then(set(draft, RichText("")))
    }
}

/// One comment: the cells it is about, what it says, the replies under it, and what can be done
/// with it.
template view comment_card(w: Workbook, c: Comment, area: State<Text>) {
    state replying: Bool = false
    state reply_text: RichText = RichText("")
    state editing: Bool = false
    state draft: RichText = c.text
    ui.Card {
        row(gap: .tight, align: .center) {
            ui.Button(c.range, icon: .locate_fixed, size: .small) {
                on press: set(area, c.range)
            }
            spacer
            if let who = created(c)?.who {
                text(who.name(), role: .caption, secondary: true)
            }
            if c.resolved {
                ui.Badge("Resolved", tone: .success)
            }
        }
        if editing {
            row(gap: .tight, align: .center) {
                ui.RichInput(bind: draft, placeholder: "Edit the comment")
                ui.Button("Save", icon: .check, size: .small) {
                    on press: edit_comment(c, draft).then(set(editing, false))
                }
                ui.Button("Cancel", icon: .x, size: .small) {
                    on press: set(editing, false)
                }
            }
        } else {
            rich_text(c.text)
        }
        for r in c.replies {
            reply_row(c, r)
        }
        if replying {
            ui.RichInput(bind: reply_text, placeholder: "Reply") {
                on submit: reply(c, reply_text)
                    .then(set(reply_text, RichText("")))
                    .then(set(replying, false))
            }
        }
        row(gap: .tight) {
            ui.Button("Reply", icon: .reply, size: .small) {
                on press: set(replying, true)
            }
            ui.Button(
                if c.resolved { "Reopen" } else { "Resolve" },
                icon: .circle_check,
                size: .small,
            ) {
                on press: resolve(c)
            }
            spacer
            if created(c)?.who == invoker {
                ui.Button("Delete", icon: .trash, icon_only: true, size: .small) {
                    on press: delete_comment(w, c)
                }
            }
        }
    }
}

/// One answer to a comment.
view reply_row(c: Comment, r: Reply) {
    stack(gap: .tight) {
        row(gap: .tight, align: .center) {
            if let who = created(r)?.who {
                text(who.name(), role: .caption, secondary: true)
            }
            spacer()
            if created(r)?.who == invoker {
                ui.Button(
                    "Delete reply",
                    icon: .trash,
                    icon_only: true,
                    role: .plain,
                    size: .small,
                ) {
                    on press: delete_reply(c, r)
                }
            }
        }
        rich_text(r.text)
    }
}

/// The charts over the open sheet, each drawn from the cells it is about, and the way to chart the
/// cells the person chose.
template view charts_panel(w: Workbook, s: Sheet, area: State<Text>, book: Text) {
    if s.charts.is_empty() {
        ui.Text("No chart over this sheet yet", secondary: true)
    }
    for c in s.charts {
        chart_card(w, s, c, area, book)
    }
    row(gap: .tight) {
        ui.Button("Column chart", icon: .bar_chart, size: .small) {
            on press: add_chart(s, area, .column, "")
        }
        ui.Button("Line chart", icon: .activity, size: .small) {
            on press: add_chart(s, area, .line, "")
        }
    }
}

/// One chart, drawn from the cells it is about, with the way to change or remove it.
template view chart_card(w: Workbook, s: Sheet, c: Chart, area: State<Text>, book: Text) {
    state editing: Bool = false
    state title: Text = c.title
    state range: Text = c.range
    let cells = shown_cells(shown_sheet(values_of(book), s.name))
    let readings = readings_of(book, s.name, c.range)
    ui.Card(title: c.title) {
        if readings.is_empty() {
            ui.Text("Nothing in {c.range} to chart", secondary: true)
        } else if c.kind == .line {
            ui.Chart([.series(values: readings, label: c.title)], title: .some(c.title))
        } else {
            ui.BarChart(
                [.bars(values: readings, label: c.title)],
                categories: categories_of(cells, c.range),
                title: .some(c.title),
            )
        }
        if editing {
            stack(gap: .tight) {
                ui.TextInput(bind: title, placeholder: "Chart title")
                ui.TextInput(bind: range, placeholder: "Cells, as A1:D9")
                row(gap: .tight) {
                    ui.Button("Save", icon: .check, size: .small) {
                        on press: edit_chart(c, title: title, range: range)
                            .then(set(editing, false))
                    }
                    ui.Button("Cancel", icon: .x, size: .small) {
                        on press: set(title, c.title)
                            .then(set(range, c.range))
                            .then(set(editing, false))
                    }
                }
            }
        }
        row(gap: .tight, wrap: true) {
            ui.Button("Show the cells", icon: .locate_fixed, size: .small) {
                on press: set(area, c.range)
            }
            spacer
            ui.MenuButton("More", icon: .ellipsis, icon_only: true) {
                ui.MenuItem("Rename or move the chart…", icon: .pencil) {
                    on choose: set(title, c.title)
                        .then(set(range, c.range))
                        .then(set(editing, true))
                }
                ui.MenuItem("Chart the chosen cells", icon: .locate_fixed) {
                    on choose: edit_chart(c, range: area)
                }
                ui.MenuItem("Column chart", icon: .bar_chart, checked: c.kind == .column) {
                    on choose: edit_chart(c, kind: .some(.column))
                }
                ui.MenuItem("Line chart", icon: .activity, checked: c.kind == .line) {
                    on choose: edit_chart(c, kind: .some(.line))
                }
                ui.MenuSeparator
                ui.MenuItem("Delete the chart", icon: .trash) {
                    on choose: delete_chart(s, c)
                }
            }
            ui.Button("Delete", icon: .trash, icon_only: true, size: .small) {
                on press: delete_chart(s, c)
            }
        }
    }
}

/// The numbers a range shows, as a chart reads them: its last column, or the only column it has.
fn readings_of(book: Text, sheet: Text, range: Text) -> List<Float> {
    match book.sheet_numbers(sheet, range) {
        .ok(text) => match json.decode<List<Float>>(text) {
            .ok(numbers) => numbers,
            .err(_) => [],
        },
        .err(_) => [],
    }
}

/// What each row a chart reads is called, from the first column of its range.
fn categories_of(cells: List<List<Text>>, range: Text) -> List<Text> {
    let column = column_letter(column_index(column_of(range_start(range))))
    let top = row_of(range_start(range))
    let bottom = row_of(range_end(range))
    count_up(top, bottom).map(row -> text_at(cell_row(cells, "{column}{row}"), 2))
}

/// The shown cell for a reference.
fn cell_row(cells: List<List<Text>>, reference: Text) -> List<Text> {
    cells.find(c -> text_at(c, 1) == reference) ?? []
}

/// The dialogs: renaming a sheet, choosing its colour, and the document's settings.
// ---------------------------------------------------------------------------------------------
// The small questions a sheet asks: its name, its colour and the workbook's settings.
template view workbook_dialogs(
    w: Workbook,
    s: Sheet,
    renaming: State<Bool>,
    rename_text: State<Text>,
    coloring: State<Bool>,
    settings_open: State<Bool>,
) {
    ui.ModalDialog(
        title: "Rename the sheet",
        open: renaming,
        primary: "Rename",
        secondary: "Cancel",
        icon: .pencil,
    ) {
        on act_primary: rename_sheet(w, s, rename_text).then(set(renaming, false))
        on act_secondary: set(renaming, false)
        on dismiss: set(renaming, false)
        ui.TextInput(bind: rename_text, autofocus: true, placeholder: s.name)
    }
    ui.ModalDialog(title: "Tab colour", open: coloring, secondary: "Done", icon: .palette) {
        on act_secondary: set(coloring, false)
        on dismiss: set(coloring, false)
        ui.ColorSwatchPicker(
            swatches: tab_colors(),
            selected: s.color,
            columns: 6,
            label: "Tab colour",
        ) {
            on change(index): set_sheet_color(s, index)
        }
        ui.Button("No colour") {
            on press: clear_sheet_color(s)
        }
    }
    ui.ModalDialog(
        title: "Document settings",
        open: settings_open,
        secondary: "Done",
        icon: .settings_2,
    ) {
        on act_secondary: set(settings_open, false)
        on dismiss: set(settings_open, false)
        stack(gap: .related) {
            text("How its numbers, dates and times are written", role: .caption, secondary: true)
            money_choice(w)
            date_choice(w)
            time_choice(w)
        }
    }
}

/// The currencies the workbook's bare money format may show in.
template view money_choice(w: Workbook) {
    ui.MenuButton("Money: {currency_name(w.currency)}", icon: .wallet) {
        let currencies: List<Currency> = [.usd, .eur, .gbp, .jpy, .chf, .cad, .aud, .cny, .inr]
        for c in currencies {
            ui.MenuItem(currency_name(c), checked: c == w.currency) {
                on choose: set_settings(w, currency: .some(c))
            }
        }
    }
}

/// The date styles the workbook reads its dates in.
template view date_choice(w: Workbook) {
    ui.MenuButton("Dates: {date_name(w.date_style)}", icon: .calendar) {
        let styles: List<DateStyle> = [.language, .iso, .us, .eu, .long]
        for d in styles {
            ui.MenuItem(date_name(d), checked: d == w.date_style) {
                on choose: set_settings(w, date_style: .some(d))
            }
        }
    }
}

/// The time styles the workbook reads its times in.
template view time_choice(w: Workbook) {
    ui.MenuButton("Times: {time_name(w.time_style)}", icon: .clock) {
        let styles: List<TimeStyle> = [.language, .h24, .h12]
        for t in styles {
            ui.MenuItem(time_name(t), checked: t == w.time_style) {
                on choose: set_settings(w, time_style: .some(t))
            }
        }
    }
}

/// What a currency is called in the menu.
fn currency_name(c: Currency) -> Text {
    "{currency_code(c)}"
}

/// What a date style is called in the menu.
fn date_name(d: DateStyle) -> Text {
    match d {
        .language => "As the language writes them",
        .iso => "2026-11-23",
        .us => "11/23/2026",
        .eu => "23.11.2026",
        .long => "November 23, 2026",
    }
}

/// What a time style is called in the menu.
fn time_name(t: TimeStyle) -> Text {
    match t {
        .language => "As the language writes them",
        .h24 => "14:30",
        .h12 => "2:30 PM",
    }
}

/// The colours a tab may take.
fn tab_colors() -> List<Text> {
    [
        "#e5484d",
        "#e54d2e",
        "#f76b15",
        "#ffb224",
        "#46a758",
        "#12a594",
        "#05a2c2",
        "#3e63dd",
        "#8e4ec6",
        "#e93d82",
        "#8d8d8d",
        "#60646c",
    ]
}
