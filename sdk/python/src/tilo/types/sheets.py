"""Types for Sheets (`so.tilo.sheets`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `sheets/<resource>`, read with
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
    "ChartKind",
    "Currency",
    "DateStyle",
    "TimeStyle",
    "Notice",
    "Workbook",
    "Sheet",
    "Chart",
    "Comment",
    "Reply",
    "SheetInput",
    "BookInput",
    "RecordsRequest",
    "UpsertRequest",
    "RecordWrites",
    "Extent",
    "Sheets",
    "SetTitleArgs",
    "RenameSheetArgs",
    "DeleteSheetArgs",
    "PlaceSheetArgs",
    "MoveSheetArgs",
    "WriteCellsArgs",
    "SetCellsArgs",
    "PasteCellsArgs",
    "ClearCellsArgs",
    "FormatCellsArgs",
    "ToggleBoldArgs",
    "AutosumArgs",
    "SumSelectionArgs",
    "InsertRowsArgs",
    "DeleteRowsArgs",
    "InsertColumnsArgs",
    "DeleteColumnsArgs",
    "SortRowsArgs",
    "FillArgs",
    "FillDownArgs",
    "FillRightArgs",
    "ApplyEditArgs",
    "DeleteCommentArgs",
    "SetSettingsArgs",
    "ImportTableArgs",
    "EditCellsArgs",
    "SetSheetHiddenArgs",
    "SetSheetColorArgs",
    "ClearSheetColorArgs",
    "DeleteChartArgs",
    "ResizeColumnArgs",
    "ResizeRowArgs",
    "EditChartArgs",
    "EditBlockArgs",
    "InsertBlockArgs",
    "DeleteBlockArgs",
    "ResolveArgs",
    "EditCommentArgs",
    "DeleteReplyArgs",
    "ReplyEditBlockArgs",
    "ReplyInsertBlockArgs",
    "ReplyDeleteBlockArgs",
    "EditReplyArgs",
]

ChartKind = Literal["column", "line"]
"""What a chart draws: columns side by side, or a line over its points."""

Currency = Literal["usd", "eur", "gbp", "jpy", "chf", "cad", "aud", "cny", "inr"]
"""The currency a bare money format shows in."""

DateStyle = Literal["language", "iso", "us", "eu", "long"]
"""How a cell writes a date: the language's own, ISO, a US or European order, or spelled out."""

TimeStyle = Literal["language", "h24", "h12"]
"""How a cell writes a clock time: the language's own, 24-hour or 12-hour."""

SetTitleArgs = TypedDict("SetTitleArgs", {
    "title": "str",
})
"""The arguments of `set_title`."""

RenameSheetArgs = TypedDict("RenameSheetArgs", {
    "s": "Sheet",
    "name": "str",
})
"""The arguments of `rename_sheet`."""

DeleteSheetArgs = TypedDict("DeleteSheetArgs", {
    "s": "Sheet",
})
"""The arguments of `delete_sheet`."""

PlaceSheetArgs = TypedDict("PlaceSheetArgs", {
    "s": "Sheet",
    "before": "Sheet",
})
"""The arguments of `place_sheet`."""

MoveSheetArgs = TypedDict("MoveSheetArgs", {
    "s": "Sheet",
    "at": "int",
})
"""The arguments of `move_sheet`."""

WriteCellsArgs = TypedDict("WriteCellsArgs", {
    "sheet": "str",
    "refs": "list[str]",
    "inputs": "list[str]",
    "seen": NotRequired["str | None"],
})
"""The arguments of `write_cells`."""

SetCellsArgs = TypedDict("SetCellsArgs", {
    "s": "Sheet",
    "start": "str",
    "rows": "list[list[str]]",
    "seen": NotRequired["str | None"],
})
"""The arguments of `set_cells`."""

PasteCellsArgs = TypedDict("PasteCellsArgs", {
    "s": "Sheet",
    "at": "str",
})
"""The arguments of `paste_cells`."""

ClearCellsArgs = TypedDict("ClearCellsArgs", {
    "s": "Sheet",
    "area": "str",
})
"""The arguments of `clear_cells`."""

FormatCellsArgs = TypedDict("FormatCellsArgs", {
    "s": "Sheet",
    "range": "str",
    "format": NotRequired["str | None"],
    "bold": NotRequired["bool | None"],
})
"""The arguments of `format_cells`."""

ToggleBoldArgs = TypedDict("ToggleBoldArgs", {
    "s": "Sheet",
    "range": "str",
})
"""The arguments of `toggle_bold`."""

AutosumArgs = TypedDict("AutosumArgs", {
    "s": "Sheet",
    "at": "str",
})
"""The arguments of `autosum`."""

SumSelectionArgs = TypedDict("SumSelectionArgs", {
    "s": "Sheet",
    "range": "str",
})
"""The arguments of `sum_selection`."""

InsertRowsArgs = TypedDict("InsertRowsArgs", {
    "s": "Sheet",
    "at": "int",
    "count": NotRequired["int | None"],
})
"""The arguments of `insert_rows`."""

DeleteRowsArgs = TypedDict("DeleteRowsArgs", {
    "s": "Sheet",
    "rows": "list[int]",
})
"""The arguments of `delete_rows`."""

InsertColumnsArgs = TypedDict("InsertColumnsArgs", {
    "s": "Sheet",
    "at": "str",
    "count": NotRequired["int | None"],
})
"""The arguments of `insert_columns`."""

DeleteColumnsArgs = TypedDict("DeleteColumnsArgs", {
    "s": "Sheet",
    "columns": "list[str]",
})
"""The arguments of `delete_columns`."""

SortRowsArgs = TypedDict("SortRowsArgs", {
    "s": "Sheet",
    "range": "str",
    "by": "str",
    "descending": NotRequired["bool | None"],
})
"""The arguments of `sort_rows`."""

FillArgs = TypedDict("FillArgs", {
    "s": "Sheet",
    "pair": "list[str]",
})
"""The arguments of `fill`."""

FillDownArgs = TypedDict("FillDownArgs", {
    "s": "Sheet",
    "range": "str",
})
"""The arguments of `fill_down`."""

FillRightArgs = TypedDict("FillRightArgs", {
    "s": "Sheet",
    "range": "str",
})
"""The arguments of `fill_right`."""

ApplyEditArgs = TypedDict("ApplyEditArgs", {
    "change": "str",
})
"""The arguments of `apply_edit`."""

DeleteCommentArgs = TypedDict("DeleteCommentArgs", {
    "c": "Comment",
})
"""The arguments of `delete_comment`."""

SetSettingsArgs = TypedDict("SetSettingsArgs", {
    "currency": NotRequired["Currency | None"],
    "date_style": NotRequired["DateStyle | None"],
    "time_style": NotRequired["TimeStyle | None"],
})
"""The arguments of `set_settings`."""

ImportTableArgs = TypedDict("ImportTableArgs", {
    "file": "Any",
})
"""The arguments of `import_table`."""

EditCellsArgs = TypedDict("EditCellsArgs", {
    "s": "Sheet",
    "pairs": "list[str]",
})
"""The arguments of `edit_cells`."""

SetSheetHiddenArgs = TypedDict("SetSheetHiddenArgs", {
    "hidden": "bool",
})
"""The arguments of `set_sheet_hidden`."""

SetSheetColorArgs = TypedDict("SetSheetColorArgs", {
    "color": "int",
})
"""The arguments of `set_sheet_color`."""

ClearSheetColorArgs = TypedDict("ClearSheetColorArgs", {
})
"""The arguments of `clear_sheet_color`."""

DeleteChartArgs = TypedDict("DeleteChartArgs", {
    "c": "Chart",
})
"""The arguments of `delete_chart`."""

ResizeColumnArgs = TypedDict("ResizeColumnArgs", {
    "pair": "list[str]",
})
"""The arguments of `resize_column`."""

ResizeRowArgs = TypedDict("ResizeRowArgs", {
    "pair": "list[str]",
})
"""The arguments of `resize_row`."""

EditChartArgs = TypedDict("EditChartArgs", {
    "title": NotRequired["str | None"],
    "kind": NotRequired["ChartKind | None"],
    "range": NotRequired["str | None"],
})
"""The arguments of `edit_chart`."""

EditBlockArgs = TypedDict("EditBlockArgs", {
    "block": "str",
    "text": "str",
})
"""The arguments of `edit-block`."""

InsertBlockArgs = TypedDict("InsertBlockArgs", {
    "block": "str",
    "text": "str",
})
"""The arguments of `insert-block`."""

DeleteBlockArgs = TypedDict("DeleteBlockArgs", {
    "block": "str",
})
"""The arguments of `delete-block`."""

ResolveArgs = TypedDict("ResolveArgs", {
    "resolved": NotRequired["bool | None"],
})
"""The arguments of `resolve`."""

EditCommentArgs = TypedDict("EditCommentArgs", {
    "text": NotRequired["str | None"],
})
"""The arguments of `edit_comment`."""

DeleteReplyArgs = TypedDict("DeleteReplyArgs", {
    "r": "Reply",
})
"""The arguments of `delete_reply`."""

ReplyEditBlockArgs = TypedDict("ReplyEditBlockArgs", {
    "block": "str",
    "text": "str",
})
"""The arguments of `edit-block`."""

ReplyInsertBlockArgs = TypedDict("ReplyInsertBlockArgs", {
    "block": "str",
    "text": "str",
})
"""The arguments of `insert-block`."""

ReplyDeleteBlockArgs = TypedDict("ReplyDeleteBlockArgs", {
    "block": "str",
})
"""The arguments of `delete-block`."""

EditReplyArgs = TypedDict("EditReplyArgs", {
    "text": NotRequired["str | None"],
})
"""The arguments of `edit_reply`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.sheets@8758f1a47a1af40a"
    @property
    def notice_to(self) -> str:
        """to (`notice-to` on the wire)."""
        return self.fields["notice-to"]

    @notice_to.setter
    def notice_to(self, value: str) -> None:
        self.edit("notice-to", value=value)
    @property
    def notice_title(self) -> str:
        """title (`notice-title` on the wire)."""
        return self.fields["notice-title"]

    @notice_title.setter
    def notice_title(self, value: str) -> None:
        self.edit("notice-title", value=value)
    @property
    def notice_body(self) -> str:
        """body (`notice-body` on the wire)."""
        return self.fields["notice-body"]

    @notice_body.setter
    def notice_body(self, value: str) -> None:
        self.edit("notice-body", value=value)
    @property
    def notice_opens(self) -> Any:
        """opens (`notice-opens` on the wire)."""
        return self.fields["notice-opens"]

    @notice_opens.setter
    def notice_opens(self, value: Any) -> None:
        self.edit("notice-opens", value=value)
    @property
    def notice_seen(self) -> bool:
        """seen (`notice-seen` on the wire)."""
        return self.fields["notice-seen"]

    @notice_seen.setter
    def notice_seen(self, value: bool) -> None:
        self.edit("notice-seen", value=value)
    @property
    def notice_device(self) -> str:
        """device (`notice-device` on the wire)."""
        return self.fields["notice-device"]

    @notice_device.setter
    def notice_device(self, value: str) -> None:
        self.edit("notice-device", value=value)
    @property
    def notice_actions(self) -> list[str]:
        """actions (`notice-actions` on the wire)."""
        return self.fields["notice-actions"]

    @notice_actions.setter
    def notice_actions(self, value: list[str]) -> None:
        self.edit("notice-actions", value=value)


class Workbook(Resource):
    """Workbook — `tl.so.tilo.sheets.Workbook`."""
    __surface__ = "so.tilo.sheets@8758f1a47a1af40a"
    meovofcz: str  # title
    pkbcb37f: list[Entry[Sheet]]  # sheets
    xwh3fsa2: list[Entry[Comment]]  # comments
    r7ipin2u: Currency  # currency
    @property
    def _33jfkdmv(self) -> DateStyle:
        """date_style (`33jfkdmv` on the wire)."""
        return self.fields["33jfkdmv"]

    @_33jfkdmv.setter
    def _33jfkdmv(self, value: DateStyle) -> None:
        self.edit("33jfkdmv", value=value)
    wvlwcqju: TimeStyle  # time_style
    qetlxegv: int  # structure

    def read_cells(self,
        *,
        sheet: str,
        range: str | None = None,
        formulas: bool | None = None,
    ) -> list[str]:
        """What the cells of `range` of the sheet named `sheet` show, row by row from its first cell to its last, or their input as typed when `formulas`: what a person reads in the grid, for an agent that never opened the window."""
        args: dict[str, Any] = {}
        args["sheet"] = sheet
        if range is not None:
            args["range"] = range
        if formulas is not None:
            args["formulas"] = formulas
        return self.action("read_cells", **args)

    def new_sheet(self,
        *,
        name: str | None = None,
    ) -> Sheet:
        """Adds a sheet at the end, named `name` or the next free `SheetN`."""
        args: dict[str, Any] = {}
        if name is not None:
            args["name"] = name
        return self.action("new_sheet", **args)

    def new_sheet_after(self,
        *,
        after: Sheet,
        name: str | None = None,
    ) -> Sheet:
        """Adds a sheet right after `after`, so a new tab stands where the person is."""
        args: dict[str, Any] = {}
        args["after"] = after
        if name is not None:
            args["name"] = name
        return self.action("new_sheet_after", **args)

    def set_title(self, *, title: str) -> None:
        """Names the workbook, so it is found and listed by that name."""
        return self.action("set_title", title=title)

    def set_title_many(self, items: list[SetTitleArgs]) -> list[dict[str, Any]]:
        """Runs `set_title` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_title", items)

    def rename_sheet(self,
        *,
        s: Sheet,
        name: str,
    ) -> None:
        """Renames a sheet; every formula that reads it follows it to its new name."""
        return self.action("rename_sheet", s=s, name=name)

    def rename_sheet_many(self, items: list[RenameSheetArgs]) -> list[dict[str, Any]]:
        """Runs `rename_sheet` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename_sheet", items)

    def duplicate_sheet(self, *, s: Sheet) -> Sheet:
        """Copies a sheet and everything on it, under a name of its own, right after it."""
        return self.action("duplicate_sheet", s=s)

    def delete_sheet(self, *, s: Sheet) -> None:
        """Deletes a sheet and what was said about it."""
        return self.action("delete_sheet", s=s)

    def delete_sheet_many(self, items: list[DeleteSheetArgs]) -> list[dict[str, Any]]:
        """Runs `delete_sheet` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_sheet", items)

    def place_sheet(self,
        *,
        s: Sheet,
        before: Sheet,
    ) -> None:
        """Puts a sheet's tab right before another, so the person reorders the tabs."""
        return self.action("place_sheet", s=s, before=before)

    def place_sheet_many(self, items: list[PlaceSheetArgs]) -> list[dict[str, Any]]:
        """Runs `place_sheet` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("place_sheet", items)

    def move_sheet(self,
        *,
        s: Sheet,
        at: int,
    ) -> None:
        """Moves a sheet's tab to `at`, the place among the tabs a person sees that a drag reports."""
        return self.action("move_sheet", s=s, at=at)

    def move_sheet_many(self, items: list[MoveSheetArgs]) -> list[dict[str, Any]]:
        """Runs `move_sheet` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_sheet", items)

    def write_cells(self,
        *,
        sheet: str,
        refs: list[str],
        inputs: list[str],
        seen: str | None = None,
    ) -> None:
        """Writes the inputs `inputs` into the cells `refs`, by position: an empty input clears its cell. A typed entry keeps the format it is read in (a date, money, a percent), unless the cell has one the person set. A `seen` stamp refuses the write when rows, columns or sheets have moved since it was read."""
        args: dict[str, Any] = {}
        args["sheet"] = sheet
        args["refs"] = refs
        args["inputs"] = inputs
        if seen is not None:
            args["seen"] = seen
        return self.action("write_cells", **args)

    def write_cells_many(self, items: list[WriteCellsArgs]) -> list[dict[str, Any]]:
        """Runs `write_cells` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("write_cells", items)

    def set_cells(self,
        *,
        s: Sheet,
        start: str,
        rows: list[list[str]],
        seen: str | None = None,
    ) -> None:
        """Sets cells from a start cell, row by row: what a table handed to the workbook writes."""
        args: dict[str, Any] = {}
        args["s"] = s
        args["start"] = start
        args["rows"] = rows
        if seen is not None:
            args["seen"] = seen
        return self.action("set_cells", **args)

    def set_cells_many(self, items: list[SetCellsArgs]) -> list[dict[str, Any]]:
        """Runs `set_cells` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_cells", items)

    def set_records(self,
        *,
        s: Sheet,
        records: list[dict[str, str]],
        row: int | None = None,
        columns: list[str] | None = None,
        key: str | None = None,
    ) -> Any:
        """Writes records into a sheet by column header: each record an object keyed by the sheet's headers, so a value a record leaves out leaves its cell instead of shifting the row after it. `row` is the first data row (the row under the headers when left out), `columns` the headers a sheet that has none starts with, and `key` the header that identifies a record's row rather than writing a new one."""
        args: dict[str, Any] = {}
        args["s"] = s
        args["records"] = records
        if row is not None:
            args["row"] = row
        if columns is not None:
            args["columns"] = columns
        if key is not None:
            args["key"] = key
        return self.action("set_records", **args)

    def upsert_item(self,
        *,
        s: Sheet,
        matching: dict[str, str],
        value: dict[str, str],
    ) -> Any:
        """Writes a record into a sheet by the values that identify its row: `matching` names the headers and values that find the row, and `value` the cells it takes. A row nothing matches is made under the headers."""
        return self.action("upsert_item", s=s, matching=matching, value=value)

    def cut_cells(self,
        *,
        s: Sheet,
        area: str,
        text: str,
    ) -> None:
        """Cuts the cells a person chose: copies them as `copy_cells` does, then clears them."""
        return self.action("cut_cells", s=s, area=area, text=text)

    def paste_cells(self,
        *,
        s: Sheet,
        at: str,
    ) -> None:
        """Writes the table on the person's clipboard into the cells from `at`: a line per row and a tab between columns. Nothing on the clipboard writes nothing."""
        return self.action("paste_cells", s=s, at=at)

    def paste_cells_many(self, items: list[PasteCellsArgs]) -> list[dict[str, Any]]:
        """Runs `paste_cells` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("paste_cells", items)

    def clear_cells(self,
        *,
        s: Sheet,
        area: str,
    ) -> None:
        """Clears the cells `area` names."""
        return self.action("clear_cells", s=s, area=area)

    def clear_cells_many(self, items: list[ClearCellsArgs]) -> list[dict[str, Any]]:
        """Runs `clear_cells` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_cells", items)

    def format_cells(self,
        *,
        s: Sheet,
        range: str,
        format: str | None = None,
        bold: bool | None = None,
    ) -> None:
        """Sets the format of the cells `range` (a code such as `currency:EUR`, `percent` or `date:ISO`), and their weight when `bold` is given."""
        args: dict[str, Any] = {}
        args["s"] = s
        args["range"] = range
        if format is not None:
            args["format"] = format
        if bold is not None:
            args["bold"] = bold
        return self.action("format_cells", **args)

    def format_cells_many(self, items: list[FormatCellsArgs]) -> list[dict[str, Any]]:
        """Runs `format_cells` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("format_cells", items)

    def toggle_bold(self,
        *,
        s: Sheet,
        range: str,
    ) -> None:
        """Makes the cells `range` bold, or regular when any one of them already is."""
        return self.action("toggle_bold", s=s, range=range)

    def toggle_bold_many(self, items: list[ToggleBoldArgs]) -> list[dict[str, Any]]:
        """Runs `toggle_bold` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("toggle_bold", items)

    def autosum(self,
        *,
        s: Sheet,
        at: str,
    ) -> None:
        """Writes the sum of the numbers above `at` into it, the run stopping at a blank, a word or an existing total: the AutoSum button."""
        return self.action("autosum", s=s, at=at)

    def autosum_many(self, items: list[AutosumArgs]) -> list[dict[str, Any]]:
        """Runs `autosum` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("autosum", items)

    def sum_selection(self,
        *,
        s: Sheet,
        range: str,
    ) -> None:
        """Adds up each column of the selection into the row under it."""
        return self.action("sum_selection", s=s, range=range)

    def sum_selection_many(self, items: list[SumSelectionArgs]) -> list[dict[str, Any]]:
        """Runs `sum_selection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("sum_selection", items)

    def insert_rows(self,
        *,
        s: Sheet,
        at: int,
        count: int | None = None,
    ) -> None:
        """Inserts `count` rows above the row `at`, counting from 1."""
        args: dict[str, Any] = {}
        args["s"] = s
        args["at"] = at
        if count is not None:
            args["count"] = count
        return self.action("insert_rows", **args)

    def insert_rows_many(self, items: list[InsertRowsArgs]) -> list[dict[str, Any]]:
        """Runs `insert_rows` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("insert_rows", items)

    def delete_rows(self,
        *,
        s: Sheet,
        rows: list[int],
    ) -> None:
        """Deletes the rows `rows`, counting from 1."""
        return self.action("delete_rows", s=s, rows=rows)

    def delete_rows_many(self, items: list[DeleteRowsArgs]) -> list[dict[str, Any]]:
        """Runs `delete_rows` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_rows", items)

    def insert_columns(self,
        *,
        s: Sheet,
        at: str,
        count: int | None = None,
    ) -> None:
        """Inserts `count` columns left of the column `at`, by its letters."""
        args: dict[str, Any] = {}
        args["s"] = s
        args["at"] = at
        if count is not None:
            args["count"] = count
        return self.action("insert_columns", **args)

    def insert_columns_many(self, items: list[InsertColumnsArgs]) -> list[dict[str, Any]]:
        """Runs `insert_columns` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("insert_columns", items)

    def delete_columns(self,
        *,
        s: Sheet,
        columns: list[str],
    ) -> None:
        """Deletes the columns `columns`, by their letters."""
        return self.action("delete_columns", s=s, columns=columns)

    def delete_columns_many(self, items: list[DeleteColumnsArgs]) -> list[dict[str, Any]]:
        """Runs `delete_columns` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_columns", items)

    def sort_rows(self,
        *,
        s: Sheet,
        range: str,
        by: str,
        descending: bool | None = None,
    ) -> None:
        """Sorts the rows of `range` by the values under the column `by`, by its letters."""
        args: dict[str, Any] = {}
        args["s"] = s
        args["range"] = range
        args["by"] = by
        if descending is not None:
            args["descending"] = descending
        return self.action("sort_rows", **args)

    def sort_rows_many(self, items: list[SortRowsArgs]) -> list[dict[str, Any]]:
        """Runs `sort_rows` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("sort_rows", items)

    def fill(self,
        *,
        s: Sheet,
        pair: list[str],
    ) -> None:
        """Extends the cells `source` over `target`, as dragging the fill handle does: numbers count on, dates follow their step and a pattern of words repeats. The pair carries the cells it copies from and the cells it fills, as the grid reports them."""
        return self.action("fill", s=s, pair=pair)

    def fill_many(self, items: list[FillArgs]) -> list[dict[str, Any]]:
        """Runs `fill` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("fill", items)

    def fill_down(self,
        *,
        s: Sheet,
        range: str,
    ) -> None:
        """Fills the rows under the first row of `range` from it: the first row copied down, counting on as a dragged fill handle does."""
        return self.action("fill_down", s=s, range=range)

    def fill_down_many(self, items: list[FillDownArgs]) -> list[dict[str, Any]]:
        """Runs `fill_down` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("fill_down", items)

    def fill_right(self,
        *,
        s: Sheet,
        range: str,
    ) -> None:
        """Fills the columns right of the first column of `range` from it: the first column copied across, counting on as a dragged fill handle does."""
        return self.action("fill_right", s=s, range=range)

    def fill_right_many(self, items: list[FillRightArgs]) -> list[dict[str, Any]]:
        """Runs `fill_right` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("fill_right", items)

    def apply_edit(self, *, change: str) -> None:
        """Applies one structural change to the workbook and keeps every sheet as the engine leaves it: its cells, their formats and the cells shown bold, its name when the change renamed it, and the cells its charts and comments are about."""
        return self.action("apply_edit", change=change)

    def apply_edit_many(self, items: list[ApplyEditArgs]) -> list[dict[str, Any]]:
        """Runs `apply_edit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("apply_edit", items)

    def comment(self,
        *,
        sheet: str,
        range: str,
        text: str,
    ) -> Comment:
        """Says `text` about the cells `range` of a sheet."""
        return self.action("comment", sheet=sheet, range=range, text=text)

    def delete_comment(self, *, c: Comment) -> None:
        """Deletes a comment and its replies. Only its author may."""
        return self.action("delete_comment", c=c)

    def delete_comment_many(self, items: list[DeleteCommentArgs]) -> list[dict[str, Any]]:
        """Runs `delete_comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_comment", items)

    def set_settings(self,
        *,
        currency: Currency | None = None,
        date_style: DateStyle | None = None,
        time_style: TimeStyle | None = None,
    ) -> None:
        """Sets the workbook's currency, date style and time style: what a cell's bare money, date and time formats are read in."""
        args: dict[str, Any] = {}
        if currency is not None:
            args["currency"] = currency
        if date_style is not None:
            args["date_style"] = date_style
        if time_style is not None:
            args["time_style"] = time_style
        return self.action("set_settings", **args)

    def set_settings_many(self, items: list[SetSettingsArgs]) -> list[dict[str, Any]]:
        """Runs `set_settings` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_settings", items)

    def import_table(self, *, file: Any) -> None:
        """Imports a table, a workbook or a Notion export as new sheets after the ones the workbook holds."""
        return self.action("import_table", file=file)

    def import_table_many(self, items: list[ImportTableArgs]) -> list[dict[str, Any]]:
        """Runs `import_table` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import_table", items)

    def read_content(self) -> str:
        """The workbook as the engine reads it, as text: its sheets, its cells as typed and the currency its bare money format shows in. An agent reads a sheet's content without the window."""
        return self.action("read_content")

    def export_workbook(self) -> Any:
        """The workbook as an Excel file."""
        return self.action("export_workbook")

    def export_ods(self) -> Any:
        """The workbook as an OpenDocument spreadsheet."""
        return self.action("export_ods")

    def export_table(self, *, s: Sheet) -> Any:
        """One sheet as a table file."""
        return self.action("export_table", s=s)

    def edit_cells(self,
        *,
        s: Sheet,
        pairs: list[str],
    ) -> None:
        """Writes the cells a grid edit reports: pairs of reference and input, in order."""
        return self.action("edit_cells", s=s, pairs=pairs)

    def edit_cells_many(self, items: list[EditCellsArgs]) -> list[dict[str, Any]]:
        """Runs `edit_cells` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("edit_cells", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"read_cells", "new_sheet", "new_sheet_after", "set_title", "set_title_many", "rename_sheet", "rename_sheet_many", "duplicate_sheet", "delete_sheet", "delete_sheet_many", "place_sheet", "place_sheet_many", "move_sheet", "move_sheet_many", "write_cells", "write_cells_many", "set_cells", "set_cells_many", "set_records", "upsert_item", "cut_cells", "paste_cells", "paste_cells_many", "clear_cells", "clear_cells_many", "format_cells", "format_cells_many", "toggle_bold", "toggle_bold_many", "autosum", "autosum_many", "sum_selection", "sum_selection_many", "insert_rows", "insert_rows_many", "delete_rows", "delete_rows_many", "insert_columns", "insert_columns_many", "delete_columns", "delete_columns_many", "sort_rows", "sort_rows_many", "fill", "fill_many", "fill_down", "fill_down_many", "fill_right", "fill_right_many", "apply_edit", "apply_edit_many", "comment", "delete_comment", "delete_comment_many", "set_settings", "set_settings_many", "import_table", "import_table_many", "read_content", "export_workbook", "export_ods", "export_table", "edit_cells", "edit_cells_many"})


class Sheet(Resource):
    """Sheet — `tl.so.tilo.sheets.Sheet`."""
    __surface__ = "so.tilo.sheets@8758f1a47a1af40a"
    l36xpi4o: str  # name
    hotvvuda: str  # source
    @property
    def _6jss3eyn(self) -> int | None:
        """color (`6jss3eyn` on the wire)."""
        return self.fields["6jss3eyn"]

    @_6jss3eyn.setter
    def _6jss3eyn(self, value: int | None) -> None:
        self.edit("6jss3eyn", value=value)
    @property
    def _6xebiw3n(self) -> dict[str, str]:
        """cells (`6xebiw3n` on the wire)."""
        return self.fields["6xebiw3n"]

    @_6xebiw3n.setter
    def _6xebiw3n(self, value: dict[str, str]) -> None:
        self.edit("6xebiw3n", value=value)
    s2rtlgut: dict[str, str]  # formats
    vy3vanj2: list[str]  # bold
    d2szrgfm: dict[str, str]  # widths
    km7cpljo: dict[str, str]  # heights
    lsltfxgz: list[Entry[Chart]]  # charts
    @property
    def _5lw5xfqk(self) -> bool:
        """hidden (`5lw5xfqk` on the wire)."""
        return self.fields["5lw5xfqk"]

    @_5lw5xfqk.setter
    def _5lw5xfqk(self, value: bool) -> None:
        self.edit("5lw5xfqk", value=value)

    def set_sheet_hidden(self, *, hidden: bool) -> None:
        """Hides a sheet's tab, or shows it again."""
        return self.action("set_sheet_hidden", hidden=hidden)

    def set_sheet_hidden_many(self, items: list[SetSheetHiddenArgs]) -> list[dict[str, Any]]:
        """Runs `set_sheet_hidden` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_sheet_hidden", items)

    def set_sheet_color(self, *, color: int) -> None:
        """Gives a sheet's tab a colour from the palette, by its place in it."""
        return self.action("set_sheet_color", color=color)

    def set_sheet_color_many(self, items: list[SetSheetColorArgs]) -> list[dict[str, Any]]:
        """Runs `set_sheet_color` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_sheet_color", items)

    def clear_sheet_color(self) -> None:
        """Gives a sheet's tab no colour of its own, so it takes the theme's."""
        return self.action("clear_sheet_color")

    def clear_sheet_color_many(self, items: list[ClearSheetColorArgs]) -> list[dict[str, Any]]:
        """Runs `clear_sheet_color` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_sheet_color", items)

    def add_chart(self,
        *,
        range: str,
        kind: ChartKind,
        title: str,
    ) -> Chart:
        """Charts the cells `range` as a column or a line chart, titled `title`."""
        return self.action("add_chart", range=range, kind=kind, title=title)

    def delete_chart(self, *, c: Chart) -> None:
        """Deletes a chart."""
        return self.action("delete_chart", c=c)

    def delete_chart_many(self, items: list[DeleteChartArgs]) -> list[dict[str, Any]]:
        """Runs `delete_chart` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_chart", items)

    def resize_column(self, *, pair: list[str]) -> None:
        """Keeps the width the person dragged a column to: the pair carries its letter and its width."""
        return self.action("resize_column", pair=pair)

    def resize_column_many(self, items: list[ResizeColumnArgs]) -> list[dict[str, Any]]:
        """Runs `resize_column` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("resize_column", items)

    def resize_row(self, *, pair: list[str]) -> None:
        """Keeps the height the person dragged a row to: the pair carries its number and its height."""
        return self.action("resize_row", pair=pair)

    def resize_row_many(self, items: list[ResizeRowArgs]) -> list[dict[str, Any]]:
        """Runs `resize_row` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("resize_row", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"set_sheet_hidden", "set_sheet_hidden_many", "set_sheet_color", "set_sheet_color_many", "clear_sheet_color", "clear_sheet_color_many", "add_chart", "delete_chart", "delete_chart_many", "resize_column", "resize_column_many", "resize_row", "resize_row_many"})


class Chart(Resource):
    """Chart — `tl.so.tilo.sheets.Chart`."""
    __surface__ = "so.tilo.sheets@8758f1a47a1af40a"
    iipmzvk6: str  # title
    todwcigp: ChartKind  # kind
    urs2nvtf: str  # range

    def edit_chart(self,
        *,
        title: str | None = None,
        kind: ChartKind | None = None,
        range: str | None = None,
    ) -> None:
        """Changes a chart: its title, and its kind or the cells it charts when given."""
        args: dict[str, Any] = {}
        if title is not None:
            args["title"] = title
        if kind is not None:
            args["kind"] = kind
        if range is not None:
            args["range"] = range
        return self.action("edit_chart", **args)

    def edit_chart_many(self, items: list[EditChartArgs]) -> list[dict[str, Any]]:
        """Runs `edit_chart` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("edit_chart", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"edit_chart", "edit_chart_many"})


class Comment(Resource):
    """Comment — `tl.so.tilo.sheets.Comment`."""
    __surface__ = "so.tilo.sheets@8758f1a47a1af40a"
    fhfswroi: str  # sheet
    lfxsmldw: str  # range
    @property
    def _437c6ryq(self) -> str:
        """text (`437c6ryq` on the wire)."""
        return self.fields["437c6ryq"]

    @_437c6ryq.setter
    def _437c6ryq(self, value: str) -> None:
        self.edit("437c6ryq", value=value)
    nosjmdwd: bool  # resolved
    @property
    def _64zcrzt6(self) -> list[Entry[Reply]]:
        """replies (`64zcrzt6` on the wire)."""
        return self.fields["64zcrzt6"]

    @_64zcrzt6.setter
    def _64zcrzt6(self, value: list[Entry[Reply]]) -> None:
        self.edit("64zcrzt6", value=value)

    def edit_block(self,
        *,
        block: str,
        text: str,
    ) -> None:
        """Set one paragraph of a rich-text field to what a Markdown text says."""
        return self.action("edit-block", block=block, text=text)

    def edit_block_many(self, items: list[EditBlockArgs]) -> list[dict[str, Any]]:
        """Runs `edit-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("edit-block", items)

    def insert_block(self,
        *,
        block: str,
        text: str,
    ) -> None:
        """Put a paragraph before the one the address names."""
        return self.action("insert-block", block=block, text=text)

    def insert_block_many(self, items: list[InsertBlockArgs]) -> list[dict[str, Any]]:
        """Runs `insert-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("insert-block", items)

    def delete_block(self, *, block: str) -> None:
        """Remove the paragraph the address names, and the paragraphs it holds."""
        return self.action("delete-block", block=block)

    def delete_block_many(self, items: list[DeleteBlockArgs]) -> list[dict[str, Any]]:
        """Runs `delete-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-block", items)

    def reply(self, *, text: str) -> Reply:
        """Answers a comment."""
        return self.action("reply", text=text)

    def resolve(self,
        *,
        resolved: bool | None = None,
    ) -> None:
        """Resolves a comment, or opens it again."""
        args: dict[str, Any] = {}
        if resolved is not None:
            args["resolved"] = resolved
        return self.action("resolve", **args)

    def resolve_many(self, items: list[ResolveArgs]) -> list[dict[str, Any]]:
        """Runs `resolve` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("resolve", items)

    def edit_comment(self,
        *,
        text: str | None = None,
    ) -> None:
        """Changes what a comment says. Only its author may."""
        args: dict[str, Any] = {}
        if text is not None:
            args["text"] = text
        return self.action("edit_comment", **args)

    def edit_comment_many(self, items: list[EditCommentArgs]) -> list[dict[str, Any]]:
        """Runs `edit_comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("edit_comment", items)

    def delete_reply(self, *, r: Reply) -> None:
        """Deletes a reply. Only its author may."""
        return self.action("delete_reply", r=r)

    def delete_reply_many(self, items: list[DeleteReplyArgs]) -> list[dict[str, Any]]:
        """Runs `delete_reply` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_reply", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"edit_block", "edit_block_many", "insert_block", "insert_block_many", "delete_block", "delete_block_many", "reply", "resolve", "resolve_many", "edit_comment", "edit_comment_many", "delete_reply", "delete_reply_many"})


class Reply(Resource):
    """Reply — `tl.so.tilo.sheets.Reply`."""
    __surface__ = "so.tilo.sheets@8758f1a47a1af40a"
    rehztzuy: str  # text

    def edit_block(self,
        *,
        block: str,
        text: str,
    ) -> None:
        """Set one paragraph of a rich-text field to what a Markdown text says."""
        return self.action("edit-block", block=block, text=text)

    def edit_block_many(self, items: list[ReplyEditBlockArgs]) -> list[dict[str, Any]]:
        """Runs `edit-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("edit-block", items)

    def insert_block(self,
        *,
        block: str,
        text: str,
    ) -> None:
        """Put a paragraph before the one the address names."""
        return self.action("insert-block", block=block, text=text)

    def insert_block_many(self, items: list[ReplyInsertBlockArgs]) -> list[dict[str, Any]]:
        """Runs `insert-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("insert-block", items)

    def delete_block(self, *, block: str) -> None:
        """Remove the paragraph the address names, and the paragraphs it holds."""
        return self.action("delete-block", block=block)

    def delete_block_many(self, items: list[ReplyDeleteBlockArgs]) -> list[dict[str, Any]]:
        """Runs `delete-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-block", items)

    def edit_reply(self,
        *,
        text: str | None = None,
    ) -> None:
        """Changes what a reply says. Only its author may."""
        args: dict[str, Any] = {}
        if text is not None:
            args["text"] = text
        return self.action("edit_reply", **args)

    def edit_reply_many(self, items: list[EditReplyArgs]) -> list[dict[str, Any]]:
        """Runs `edit_reply` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("edit_reply", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"edit_block", "edit_block_many", "insert_block", "insert_block_many", "delete_block", "delete_block_many", "edit_reply", "edit_reply_many"})


class SheetInput(Resource):
    """SheetInput — `tl.so.tilo.sheets.SheetInput`."""
    __surface__ = "so.tilo.sheets@8758f1a47a1af40a"
    d4n4c4j6: str  # name
    iitrsrcd: bool  # hidden
    owwnyfq7: dict[str, str]  # cells
    ftagyrff: dict[str, str]  # formats
    xrjhhdys: list[str]  # bold


class BookInput(Resource):
    """BookInput — `tl.so.tilo.sheets.BookInput`."""
    __surface__ = "so.tilo.sheets@8758f1a47a1af40a"
    z2cfbg2g: list[SheetInput]  # sheets
    ktfpf6rx: str  # currency


class RecordsRequest(Resource):
    """RecordsRequest — `tl.so.tilo.sheets.RecordsRequest`."""
    __surface__ = "so.tilo.sheets@8758f1a47a1af40a"
    quawedop: list[dict[str, str]]  # records
    uslgkxn3: int | None  # row
    vstmdg7x: list[str] | None  # columns
    bou7m624: str | None  # key


class UpsertRequest(Resource):
    """UpsertRequest — `tl.so.tilo.sheets.UpsertRequest`."""
    __surface__ = "so.tilo.sheets@8758f1a47a1af40a"
    styf7w4a: list[dict[str, str]]  # records
    s3tgj4q3: dict[str, str]  # matching
    wlypu6mi: dict[str, str]  # value


class RecordWrites(Resource):
    """RecordWrites — `tl.so.tilo.sheets.RecordWrites`."""
    __surface__ = "so.tilo.sheets@8758f1a47a1af40a"
    tloxk2t5: dict[str, str]  # cells
    chuotih4: dict[str, str]  # formats


class Extent(Resource):
    """Extent — `tl.so.tilo.sheets.Extent`."""
    __surface__ = "so.tilo.sheets@8758f1a47a1af40a"
    @property
    def _5pdxnqaj(self) -> int:
        """columns (`5pdxnqaj` on the wire)."""
        return self.fields["5pdxnqaj"]

    @_5pdxnqaj.setter
    def _5pdxnqaj(self, value: int) -> None:
        self.edit("5pdxnqaj", value=value)
    hmh5rxxy: int  # rows


class Sheets(Resource):
    """Sheets — `tl.so.tilo.sheets`."""
    __surface__ = "so.tilo.sheets@8758f1a47a1af40a"
    def copy_cells(self, *, text: str) -> None:
        """Copies the cells a person chose to their clipboard as a table: a line per row and a tab between columns, which a sheet, a word processor or a mail app reads."""
        return self.action("copy_cells", text=text)

    def read_file(self, *, file: Any) -> Any:
        """What a spreadsheet file holds, as the sheets the app keeps: each sheet's name, whether its tab is hidden, and its cells as typed by their A1 reference. An agent reads a table without the window."""
        return self.action("read_file", file=file)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"copy_cells", "read_file"})


