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
sheets-add-chart = Charted { $range }
sheets-edit-chart = Edited the chart { $title }
sheets-edit-comment = Edited a comment: { $body }
sheets-delete-chart = Deleted a chart
sheets-comment = Commented on { $range }: { $text }
sheets-reply = Replied: { $text }
sheets-delete-comment = Deleted a comment
sheets-cell-raw = Edited a cell
sheets-cell-format = Formatted a cell
sheets-cell-bold = { $new ->
    [true] Made a cell bold
   *[false] Made a cell regular
}
sheets-sheet-name = Renamed { $item } to { $new }
sheets-sheet-color = Changed the tab colour of { $item }
sheets-sheet-cells = Edited the cells of { $item }
sheets-workbook-sheets = Rearranged the sheets
sheets-chart-title = Retitled a chart to { $new }
sheets-chart-kind = Changed a chart's type
sheets-comment-body = Edited a comment
sheets-comment-resolved = { $new ->
    [true] Resolved a comment
   *[false] Reopened a comment
}
sheets-reply-body = Edited a reply
sheets-doc-settings-currency = Set the workbook's currency
sheets-doc-settings-date-format = Set the workbook's date format
sheets-doc-settings-time-format = Set the workbook's time format
