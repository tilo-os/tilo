# History: what each change to the workbook did.
sheets-new-sheet = Added a sheet
sheets-duplicate-sheet = Duplicated a sheet
sheets-delete-sheet = Deleted a sheet
sheets-set-cells = Set cells from { $start }
sheets-write-cells = Wrote cells
sheets-format-cells = Formatted { $range }
sheets-sort-rows = Sorted { $range } by column { $by }
sheets-insert-rows = Inserted rows above row { $at }
sheets-delete-rows = Deleted rows
sheets-insert-columns = Inserted columns left of column { $at }
sheets-delete-columns = Deleted columns
sheets-import-sheets = Imported sheets
sheets-set-title = Named the workbook { $title }
sheets-set-records = Wrote records
sheets-upsert-item = Updated or added a row
sheets-add-chart = Charted { $range }
sheets-edit-chart = Edited the chart { $title }
sheets-edit-comment = Edit comment
sheets-delete-chart = Delete chart
sheets-comment = Commented on { $range }: { $text }
sheets-reply = Replied: { $text }
sheets-delete-comment = Delete comment
sheets-cell-raw = Edited a cell
sheets-cell-format = Formatted a cell
sheets-cell-bold = { $new ->
    [true] Made a cell bold
   *[false] Made a cell regular
}
sheets-sheet-name = Renamed { $item } to { $new }
sheets-sheet-color = Changed the tab colour of { $item }
sheets-sheet-cells = Edited the cells of { $item }
sheets-sheet-hidden = { $new ->
    [true] Hid { $item }
   *[false] Showed { $item }
}
sheets-workbook-sheets = Rearranged the sheets
sheets-workbook-title = Named the workbook { $new }
sheets-chart-title = Retitled a chart to { $new }
sheets-chart-kind = Changed a chart's type
sheets-chart-area = Charted other cells
sheets-comment-body = Edited a comment
sheets-comment-resolved = { $new ->
    [true] Resolved a comment
   *[false] Reopened a comment
}
sheets-reply-body = Edited a reply
sheets-doc-settings-currency = Set the workbook's currency
sheets-doc-settings-date-format = Set the workbook's date format
sheets-doc-settings-time-format = Set the workbook's time format

# Importing a Notion export.
sheets-notion-import-title = Import Notion export
sheets-notion-files = Notion export
sheets-notion-json-files = Notion database listing
sheets-notion-imported =
    { $tables ->
        [one] Imported one table
       *[other] Imported { $tables } tables
    }
sheets-notion-imported-table = Imported table
sheets-notion-no-tables = No table in this export to import
sheets-notion-too-big = The export is too large to import
sheets-notion-too-many-tables = The export holds more than { $limit } tables
sheets-notion-archive-invalid = The export archive cannot be read
sheets-notion-unreadable = This file is not a Notion export
sheets-notion-yes = Yes
sheets-notion-no = No
sheets-start-formula = Start a formula
sheets-start-formula-detail = Begin a formula in the active cell
sheets-name-box = Name box
sheets-name-box-detail = Go to a cell or range; shortcut Control+G
sheets-formula-bar = Formula bar
sheets-formula-bar-detail = Edit the active cell's value or formula
sheets-add-sheet = Add sheet
sheets-column-headers = Column headers
sheets-comments-charts = Comments and charts
sheets-manage-chart = Manage chart
sheets-open-chart-range = Open chart range
sheets-rename-chart = Rename chart
sheets-column-chart = Column chart
sheets-line-chart = Line chart
sheets-chart-cells = Chart cells
sheets-reply-comment = Reply to comment
sheets-open-comment-anchor = Open comment anchor
sheets-resolve-comment = Resolve comment
sheets-reopen-comment = Reopen comment
sheets-actions = Spreadsheet actions
sheets-window-name = Sheets
sheets-window-detail = Editable workbook with formula bar and sheet tabs

## Window and accessibility copy
sheets-numbers-none = No numbers in this range

## Facts, property labels and section titles
sheets-a11y-comment-by = Comment by { $author }
sheets-a11y-chart-extent = { $start }:{ $end }; open, rename, change its type or cells, or delete from the chart card
sheets-a11y-worksheet = Worksheet; selected range { $range }; { $columns } columns by { $rows } rows; only the visible grid window is announced
sheets-a11y-row = Row { $row }
sheets-a11y-cell = Column { $column }, row { $row }
sheets-a11y-column = Column { $letter }

## What the window shows
sheets-no-replies = No replies

sheets-comment-you = You
sheets-comment-someone = Someone

## The document settings sheet
sheets-document-settings = Document settings
sheets-done = Done

## The window bar's slots
sheets-file-menu = File
sheets-format-menu = Format
sheets-bold = Bold
sheets-autosum = AutoSum
sheets-show-formulas = Show formulas
sheets-show-results = Show calculated results

## A tab's colour mark
sheets-tab-colour = Tab colour
