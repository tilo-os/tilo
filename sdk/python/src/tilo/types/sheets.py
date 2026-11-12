"""Types for Sheets (`so.tilo.sheets`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Workbook` is the app's `sheets.workbook` at `sheets/<resource>`, read with
`await ….load()`: its fields are the app's fields (read-only ones without a setter)
and its methods are its actions. A method answers what the app declares; an action
that takes longer answers a `Run`. A record is the dict the wire carries, and an
enum whose cases carry fields is an object whose tag the app's schema names. An
action whose name is one the class already has is run with `action("…")`, which
the file says where it is.
"""

from __future__ import annotations

from typing import Any, Literal, TypedDict
from tilo import Resource, Run

__all__ = [
    "Area",
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
    "TimeStyle",
    "Workbook",
]

class Area(TypedDict):
    top: str
    left: str
    bottom: str
    right: str

class CellWrite(TypedDict):
    cell: str
    input: str | None
    format: Any | None
    bold: bool | None

class Chart(TypedDict):
    title: str
    kind: ChartKind
    area: Area

ChartKind = Literal["column", "line"]

ClipAttr = dict[str, Any]
"""One of bool, int, str: an object whose case the app tags it with."""

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
    author: str
    date: str
    text: str
    body: list[ClipBlock]
    resolved: bool
    replies: list[Reply]

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
"""One of bool, int, str: an object whose case the app tags it with."""

class Reply(TypedDict):
    author: str
    date: str
    text: str
    body: list[ClipBlock]

class Sheet(TypedDict):
    name: str
    source: str | None
    color: int | None
    cells: Any
    charts: list[Chart]
    hidden: bool

TimeStyle = Literal["Language", "H24", "H12"]

class Workbook(Resource):
    """Workbook — `sheets.workbook`."""
    __surface__ = "so.tilo.sheets@9ade0694b6a2e150"
    sheets: list[Sheet]
    comments: list[Comment]
    settings: DocSettings
    title: str | None

    def new_sheet(self,
        *,
        name: str | None = None,
        after: str | None = None,
        rows: list[list[Any]] | None = None,
        columns: list[str] | None = None,
        records: list[Any] | None = None,
        formats: Any | None = None,
        workbook: str | None = None,
    ) -> Any | Run:
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

    def set_title(self, *, title: str) -> Run | None:
        """Runs the app's action."""
        return self.action("set-title", title=title)

    def set_records(self,
        *,
        sheet: str,
        records: list[Any],
        row: int | None = None,
        columns: list[str] | None = None,
        key: str | None = None,
    ) -> Any | Run:
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
    ) -> Any | Run:
        """Runs the app's action."""
        return self.action("upsert-item", sheet=sheet, match=match, value=value)

    def duplicate_sheet(self, *, sheet: str) -> Run | None:
        """Runs the app's action."""
        return self.action("duplicate-sheet", sheet=sheet)

    def delete_sheet(self, *, sheet: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-sheet", sheet=sheet)

    def set_cells(self,
        *,
        sheet: str,
        start: str,
        rows: list[list[Any]],
    ) -> Any | Run:
        """Runs the app's action."""
        return self.action("set-cells", sheet=sheet, start=start, rows=rows)

    def write_cells(self,
        *,
        sheet: str,
        cells: list[CellWrite],
    ) -> Any | Run:
        """Runs the app's action."""
        return self.action("write-cells", sheet=sheet, cells=cells)

    def format_cells(self,
        *,
        sheet: str,
        range: str,
        format: Any | None = None,
        bold: bool | None = None,
    ) -> Any | Run:
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
    ) -> Any | Run:
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
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["sheet"] = sheet
        args["at"] = at
        if count is not None:
            args["count"] = count
        return self.action("insert-rows", **args)

    def delete_rows(self,
        *,
        sheet: str,
        rows: list[int],
    ) -> Any | Run:
        """Runs the app's action."""
        return self.action("delete-rows", sheet=sheet, rows=rows)

    def insert_columns(self,
        *,
        sheet: str,
        at: str,
        count: int | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["sheet"] = sheet
        args["at"] = at
        if count is not None:
            args["count"] = count
        return self.action("insert-columns", **args)

    def delete_columns(self,
        *,
        sheet: str,
        columns: list[str],
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-columns", sheet=sheet, columns=columns)

    def import_sheets(self,
        *,
        sheets: list[ImportedSheet],
        source: str | None = None,
        title: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["sheets"] = sheets
        if source is not None:
            args["source"] = source
        if title is not None:
            args["title"] = title
        return self.action("import-sheets", **args)

    def add_chart(self,
        *,
        sheet: str,
        range: str,
        kind: str,
        title: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("add-chart", sheet=sheet, range=range, kind=kind, title=title)

    def edit_chart(self,
        *,
        sheet: str,
        chart: str,
        title: str,
        kind: Any | None = None,
        range: Any | None = None,
    ) -> Run | None:
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

    def delete_chart(self,
        *,
        sheet: str,
        chart: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-chart", sheet=sheet, chart=chart)

    def rename_sheet(self,
        *,
        sheet: str,
        name: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("rename-sheet", sheet=sheet, name=name)

    def comment(self,
        *,
        sheet: str,
        range: str,
        text: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("comment", sheet=sheet, range=range, text=text)

    def reply(self,
        *,
        comment: str,
        text: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("reply", comment=comment, text=text)

    def edit_comment(self,
        *,
        comment: str,
        body: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("edit-comment", comment=comment, body=body)

    def delete_comment(self, *, comment: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-comment", comment=comment)

    def export_xlsx_content(self) -> None:
        """Runs the app's action."""
        return self.action("export-xlsx-content")


