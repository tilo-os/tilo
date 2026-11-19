"""Types for Sheets (`so.tilo.sheets`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Workbook` is the app's `sheets.workbook` at `sheets/<resource>`, read with
`await ….load()`: its fields are the app's fields (read-only ones without a setter)
and its methods are its actions. A method answers what the app declares; an action
that takes longer answers an `Invocation`. A record is the dict the wire carries, and an
enum whose cases carry fields is an object whose tag the app's schema names. An
action whose name is one the class already has is run with `action("…")`, which
the file says where it is. An undoable action that answers nothing also has a
`<action>_many`, which applies it to every argument object of a list in a few
all-or-nothing steps, all of them one invocation group.
"""

from __future__ import annotations

from typing import Any, Literal, NotRequired, TypedDict
from tilo import Entry, Invocation, Resource

__all__ = [
    "Area",
    "Author",
    "CellWrite",
    "Chart",
    "ChartKind",
    "ClipAttr",
    "ClipBlock",
    "Comment",
    "CommentCells",
    "Currency",
    "DateStyle",
    "DocSettings",
    "ImportedSheet",
    "MarkSpan",
    "MarkValue",
    "Reply",
    "Sheet",
    "SheetSpec",
    "TimeStyle",
    "Workbook",
    "SetTitleArgs",
    "DuplicateSheetArgs",
    "DeleteSheetArgs",
    "InsertRowsArgs",
    "InsertColumnsArgs",
    "DeleteColumnsArgs",
    "ImportSheetsArgs",
    "AddChartArgs",
    "EditChartArgs",
    "DeleteChartArgs",
    "RenameSheetArgs",
    "CommentArgs",
    "ReplyArgs",
    "EditCommentArgs",
    "DeleteCommentArgs",
]

class Area(TypedDict):
    top: str
    left: str
    bottom: str
    right: str

Author = dict[str, Any]
"""One of member, named: an object whose case the app tags it with."""

class CellWrite(TypedDict):
    cell: str
    input: str | None
    format: Any | None
    bold: bool | None

class Chart(TypedDict):
    title: str
    kind: ChartKind
    area: Area

ChartKind = Literal["Column", "Line"]

ClipAttr = dict[str, Any]
"""One of Bool, Int, Str: an object whose case the app tags it with."""

class ClipBlock(TypedDict):
    kind: str
    text: str
    marks: list[MarkSpan]
    level: int | None
    ordered: bool | None
    align: str | None
    attrs: dict[str, ClipAttr]

class Comment(TypedDict):
    anchor: CommentCells
    author: Author
    date: str
    text: str
    body: list[ClipBlock]
    resolved: bool
    replies: list[Entry[Reply]]

class CommentCells(TypedDict):
    sheet: str
    area: Area

Currency = Literal["USD", "EUR", "GBP", "JPY", "CHF", "CAD", "AUD", "CNY", "INR"]

DateStyle = Literal["Language", "ISO", "US", "EU", "Long"]

class DocSettings(TypedDict):
    currency: Currency | None
    date_format: DateStyle | None
    time_format: TimeStyle | None

class ImportedSheet(TypedDict):
    name: str
    cells: list[CellWrite]
    hidden: bool

class MarkSpan(TypedDict):
    start: int
    end: int
    name: str
    value: MarkValue

MarkValue = dict[str, Any]
"""One of Bool, Int, Str: an object whose case the app tags it with."""

class Reply(TypedDict):
    author: Author
    date: str
    text: str
    body: list[ClipBlock]

class Sheet(TypedDict):
    name: str
    source: str | None
    color: int | None
    cells: Any
    charts: list[Entry[Chart]]
    hidden: bool
    cut: list[str]

class SheetSpec(TypedDict):
    name: str | None
    after: str | None
    rows: list[list[Any]] | None
    columns: list[str] | None
    records: list[Any] | None
    formats: dict[str, str] | None
    workbook: str | None

TimeStyle = Literal["Language", "H24", "H12"]

SetTitleArgs = TypedDict("SetTitleArgs", {
    "title": "str",
})
"""The arguments of `set-title`."""

DuplicateSheetArgs = TypedDict("DuplicateSheetArgs", {
    "sheet": "str",
})
"""The arguments of `duplicate-sheet`."""

DeleteSheetArgs = TypedDict("DeleteSheetArgs", {
    "sheet": "str",
})
"""The arguments of `delete-sheet`."""

InsertRowsArgs = TypedDict("InsertRowsArgs", {
    "sheet": "str",
    "at": "int",
    "count": NotRequired["int | None"],
})
"""The arguments of `insert-rows`."""

InsertColumnsArgs = TypedDict("InsertColumnsArgs", {
    "sheet": "str",
    "at": "str",
    "count": NotRequired["int | None"],
})
"""The arguments of `insert-columns`."""

DeleteColumnsArgs = TypedDict("DeleteColumnsArgs", {
    "sheet": "str",
    "columns": "list[str]",
})
"""The arguments of `delete-columns`."""

ImportSheetsArgs = TypedDict("ImportSheetsArgs", {
    "sheets": "list[ImportedSheet]",
    "source": NotRequired["str | None"],
    "title": NotRequired["str | None"],
})
"""The arguments of `import-sheets`."""

AddChartArgs = TypedDict("AddChartArgs", {
    "sheet": "str",
    "range": "str",
    "kind": "str",
    "title": "str",
})
"""The arguments of `add-chart`."""

EditChartArgs = TypedDict("EditChartArgs", {
    "sheet": "str",
    "chart": "str",
    "title": "str",
    "kind": NotRequired["Any | None"],
    "range": NotRequired["Any | None"],
})
"""The arguments of `edit-chart`."""

DeleteChartArgs = TypedDict("DeleteChartArgs", {
    "sheet": "str",
    "chart": "str",
})
"""The arguments of `delete-chart`."""

RenameSheetArgs = TypedDict("RenameSheetArgs", {
    "sheet": "str",
    "name": "str",
})
"""The arguments of `rename-sheet`."""

CommentArgs = TypedDict("CommentArgs", {
    "sheet": "str",
    "range": "str",
    "text": "str",
})
"""The arguments of `comment`."""

ReplyArgs = TypedDict("ReplyArgs", {
    "comment": "str",
    "text": "str",
})
"""The arguments of `reply`."""

EditCommentArgs = TypedDict("EditCommentArgs", {
    "comment": "str",
    "body": "str",
})
"""The arguments of `edit-comment`."""

DeleteCommentArgs = TypedDict("DeleteCommentArgs", {
    "comment": "str",
})
"""The arguments of `delete-comment`."""

class Workbook(Resource):
    """Workbook — `sheets.workbook`."""
    __surface__ = "so.tilo.sheets@f729b94191b1093c"
    sheets: list[Entry[Sheet]]
    comments: list[Entry[Comment]]
    settings: DocSettings
    title: str | None

    def new_sheet(self,
        *,
        name: str | None = None,
        after: str | None = None,
        rows: list[list[Any]] | None = None,
        columns: list[str] | None = None,
        records: list[Any] | None = None,
        formats: dict[str, str] | None = None,
        workbook: str | None = None,
    ) -> Any:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if name is not None:
            args["name"] = name
        if after is not None:
            args["after"] = after
        if rows is not None:
            args["rows"] = rows
        if columns is not None:
            args["columns"] = columns
        if records is not None:
            args["records"] = records
        if formats is not None:
            args["formats"] = formats
        if workbook is not None:
            args["workbook"] = workbook
        return self.action("new-sheet", **args)

    def set_title(self, *, title: str) -> None:
        """Runs the app's action."""
        return self.action("set-title", title=title)

    def set_title_many(self, items: list[SetTitleArgs]) -> list[dict[str, Any]]:
        """Runs `set-title` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-title", items)

    def set_records(self,
        *,
        sheet: str,
        records: list[Any],
        row: int | None = None,
        columns: list[str] | None = None,
        key: str | None = None,
    ) -> Any:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["sheet"] = sheet
        args["records"] = records
        if row is not None:
            args["row"] = row
        if columns is not None:
            args["columns"] = columns
        if key is not None:
            args["key"] = key
        return self.action("set-records", **args)

    def upsert_item(self,
        *,
        sheet: str,
        match: dict[str, Any],
        value: dict[str, Any],
    ) -> Any:
        """Runs the app's action."""
        return self.action("upsert-item", sheet=sheet, match=match, value=value)

    def duplicate_sheet(self, *, sheet: str) -> None:
        """Runs the app's action."""
        return self.action("duplicate-sheet", sheet=sheet)

    def duplicate_sheet_many(self, items: list[DuplicateSheetArgs]) -> list[dict[str, Any]]:
        """Runs `duplicate-sheet` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("duplicate-sheet", items)

    def delete_sheet(self, *, sheet: str) -> None:
        """Runs the app's action."""
        return self.action("delete-sheet", sheet=sheet)

    def delete_sheet_many(self, items: list[DeleteSheetArgs]) -> list[dict[str, Any]]:
        """Runs `delete-sheet` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-sheet", items)

    def set_cells(self,
        *,
        sheet: str,
        start: str,
        rows: list[list[Any]],
        seen: str | None = None,
    ) -> Any:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["sheet"] = sheet
        args["start"] = start
        args["rows"] = rows
        if seen is not None:
            args["seen"] = seen
        return self.action("set-cells", **args)

    def write_cells(self,
        *,
        sheet: str,
        cells: list[CellWrite],
        seen: str | None = None,
    ) -> Any:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["sheet"] = sheet
        args["cells"] = cells
        if seen is not None:
            args["seen"] = seen
        return self.action("write-cells", **args)

    def format_cells(self,
        *,
        sheet: str,
        range: str,
        format: Any | None = None,
        bold: bool | None = None,
    ) -> Any:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["sheet"] = sheet
        args["range"] = range
        if format is not None:
            args["format"] = format
        if bold is not None:
            args["bold"] = bold
        return self.action("format-cells", **args)

    def sort_rows(self,
        *,
        sheet: str,
        range: str,
        by: str,
        descending: bool | None = None,
    ) -> Any:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["sheet"] = sheet
        args["range"] = range
        args["by"] = by
        if descending is not None:
            args["descending"] = descending
        return self.action("sort-rows", **args)

    def insert_rows(self,
        *,
        sheet: str,
        at: int,
        count: int | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["sheet"] = sheet
        args["at"] = at
        if count is not None:
            args["count"] = count
        return self.action("insert-rows", **args)

    def insert_rows_many(self, items: list[InsertRowsArgs]) -> list[dict[str, Any]]:
        """Runs `insert-rows` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("insert-rows", items)

    def delete_rows(self,
        *,
        sheet: str,
        rows: list[int],
    ) -> Any:
        """Runs the app's action."""
        return self.action("delete-rows", sheet=sheet, rows=rows)

    def insert_columns(self,
        *,
        sheet: str,
        at: str,
        count: int | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["sheet"] = sheet
        args["at"] = at
        if count is not None:
            args["count"] = count
        return self.action("insert-columns", **args)

    def insert_columns_many(self, items: list[InsertColumnsArgs]) -> list[dict[str, Any]]:
        """Runs `insert-columns` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("insert-columns", items)

    def delete_columns(self,
        *,
        sheet: str,
        columns: list[str],
    ) -> None:
        """Runs the app's action."""
        return self.action("delete-columns", sheet=sheet, columns=columns)

    def delete_columns_many(self, items: list[DeleteColumnsArgs]) -> list[dict[str, Any]]:
        """Runs `delete-columns` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-columns", items)

    def import_sheets(self,
        *,
        sheets: list[ImportedSheet],
        source: str | None = None,
        title: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["sheets"] = sheets
        if source is not None:
            args["source"] = source
        if title is not None:
            args["title"] = title
        return self.action("import-sheets", **args)

    def import_sheets_many(self, items: list[ImportSheetsArgs]) -> list[dict[str, Any]]:
        """Runs `import-sheets` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import-sheets", items)

    def add_chart(self,
        *,
        sheet: str,
        range: str,
        kind: str,
        title: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("add-chart", sheet=sheet, range=range, kind=kind, title=title)

    def add_chart_many(self, items: list[AddChartArgs]) -> list[dict[str, Any]]:
        """Runs `add-chart` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-chart", items)

    def edit_chart(self,
        *,
        sheet: str,
        chart: str,
        title: str,
        kind: Any | None = None,
        range: Any | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["sheet"] = sheet
        args["chart"] = chart
        args["title"] = title
        if kind is not None:
            args["kind"] = kind
        if range is not None:
            args["range"] = range
        return self.action("edit-chart", **args)

    def edit_chart_many(self, items: list[EditChartArgs]) -> list[dict[str, Any]]:
        """Runs `edit-chart` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("edit-chart", items)

    def delete_chart(self,
        *,
        sheet: str,
        chart: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("delete-chart", sheet=sheet, chart=chart)

    def delete_chart_many(self, items: list[DeleteChartArgs]) -> list[dict[str, Any]]:
        """Runs `delete-chart` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-chart", items)

    def rename_sheet(self,
        *,
        sheet: str,
        name: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("rename-sheet", sheet=sheet, name=name)

    def rename_sheet_many(self, items: list[RenameSheetArgs]) -> list[dict[str, Any]]:
        """Runs `rename-sheet` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename-sheet", items)

    def comment(self,
        *,
        sheet: str,
        range: str,
        text: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("comment", sheet=sheet, range=range, text=text)

    def comment_many(self, items: list[CommentArgs]) -> list[dict[str, Any]]:
        """Runs `comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("comment", items)

    def reply(self,
        *,
        comment: str,
        text: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("reply", comment=comment, text=text)

    def reply_many(self, items: list[ReplyArgs]) -> list[dict[str, Any]]:
        """Runs `reply` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("reply", items)

    def edit_comment(self,
        *,
        comment: str,
        body: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("edit-comment", comment=comment, body=body)

    def edit_comment_many(self, items: list[EditCommentArgs]) -> list[dict[str, Any]]:
        """Runs `edit-comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("edit-comment", items)

    def delete_comment(self, *, comment: str) -> None:
        """Runs the app's action."""
        return self.action("delete-comment", comment=comment)

    def delete_comment_many(self, items: list[DeleteCommentArgs]) -> list[dict[str, Any]]:
        """Runs `delete-comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-comment", items)

    def export_xlsx_content(self) -> None:
        """Runs the app's action."""
        return self.action("export-xlsx-content")

    def export_csv_content(self,
        *,
        sheet: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if sheet is not None:
            args["sheet"] = sheet
        return self.action("export-csv-content", **args)

    def read_file(self,
        *,
        file: str,
        sheet: str | None = None,
        from_: int | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["file"] = file
        if sheet is not None:
            args["sheet"] = sheet
        if from_ is not None:
            args["from"] = from_
        return self.action("read-file", **args)

    def read_content(self,
        *,
        content: str,
        name: str,
        sheet: str | None = None,
        from_: int | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["content"] = content
        args["name"] = name
        if sheet is not None:
            args["sheet"] = sheet
        if from_ is not None:
            args["from"] = from_
        return self.action("read-content", **args)

    def read_cells(self,
        *,
        sheet: str,
        range: str | None = None,
        from_: int | None = None,
        shown: bool | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["sheet"] = sheet
        if range is not None:
            args["range"] = range
        if from_ is not None:
            args["from"] = from_
        if shown is not None:
            args["shown"] = shown
        return self.action("read-cells", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_sheet", "set_title", "set_title_many", "set_records", "upsert_item", "duplicate_sheet", "duplicate_sheet_many", "delete_sheet", "delete_sheet_many", "set_cells", "write_cells", "format_cells", "sort_rows", "insert_rows", "insert_rows_many", "delete_rows", "insert_columns", "insert_columns_many", "delete_columns", "delete_columns_many", "import_sheets", "import_sheets_many", "add_chart", "add_chart_many", "edit_chart", "edit_chart_many", "delete_chart", "delete_chart_many", "rename_sheet", "rename_sheet_many", "comment", "comment_many", "reply", "reply_many", "edit_comment", "edit_comment_many", "delete_comment", "delete_comment_many", "export_xlsx_content", "export_csv_content", "read_file", "read_content", "read_cells"})


