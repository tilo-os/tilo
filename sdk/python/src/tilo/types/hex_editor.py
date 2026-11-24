"""Types for Hex Editor (`so.tilo.hex-editor`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Change` is the app's `tl.so.tilo.hex-editor.Change` at `hex-editor/<resource>`, read with
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
    "Change",
    "Row",
    "Finding",
    "Span",
    "Desk",
    "Document",
    "Library",
    "OpenAsk",
    "OpenedAnswer",
    "RowsAsk",
    "RowsAnswer",
    "FindAsk",
    "FoundAnswer",
    "ApplyAsk",
    "AppliedAnswer",
    "ReplaceAsk",
    "ReplacedAnswer",
    "RangeAsk",
    "RangeAnswer",
    "BytesAsk",
    "BytesAnswer",
    "SaveAsk",
    "SavedAnswer",
    "HexEditor",
    "ShowArgs",
    "SaveArgs",
    "RebasedArgs",
    "MovedArgs",
    "TypeKeyArgs",
    "TypeHexArgs",
    "TypeAsciiArgs",
    "BackspaceArgs",
    "DeleteAtCursorArgs",
    "SelectAllArgs",
    "PasteArgs",
    "ReplacedArgs",
    "StartSearchArgs",
    "StartReplaceArgs",
    "StartGotoArgs",
    "ToggleInspectorArgs",
    "ClosePanelArgs",
    "SearchArgs",
    "RevealArgs",
    "ReplaceAllArgs",
    "ReplaceSpansArgs",
    "SearchKindArgs",
    "SearchCaseArgs",
    "SearchWordArgs",
    "GoToArgs",
    "SetEndianArgs",
    "SetModeArgs",
    "SetColumnArgs",
    "DismissArgs",
    "PressedArgs",
    "ScrolledArgs",
    "InsertBytesArgs",
    "OverwriteBytesArgs",
    "DeleteBytesArgs",
    "ForgetArgs",
]

ShowArgs = TypedDict("ShowArgs", {
})
"""The arguments of `show`."""

SaveArgs = TypedDict("SaveArgs", {
})
"""The arguments of `save`."""

RebasedArgs = TypedDict("RebasedArgs", {
    "address": "str",
    "length": "int",
})
"""The arguments of `rebased`."""

MovedArgs = TypedDict("MovedArgs", {
    "desk": "Desk",
    "at": "int",
    "extend": "bool",
})
"""The arguments of `moved`."""

TypeKeyArgs = TypedDict("TypeKeyArgs", {
    "desk": "Desk",
    "one": "str",
})
"""The arguments of `type_key`."""

TypeHexArgs = TypedDict("TypeHexArgs", {
    "desk": "Desk",
    "value": "int",
})
"""The arguments of `type_hex`."""

TypeAsciiArgs = TypedDict("TypeAsciiArgs", {
    "desk": "Desk",
    "one": "str",
})
"""The arguments of `type_ascii`."""

BackspaceArgs = TypedDict("BackspaceArgs", {
    "desk": "Desk",
})
"""The arguments of `backspace`."""

DeleteAtCursorArgs = TypedDict("DeleteAtCursorArgs", {
    "desk": "Desk",
})
"""The arguments of `delete_at_cursor`."""

SelectAllArgs = TypedDict("SelectAllArgs", {
    "desk": "Desk",
})
"""The arguments of `select_all`."""

PasteArgs = TypedDict("PasteArgs", {
})
"""The arguments of `paste`."""

ReplacedArgs = TypedDict("ReplacedArgs", {
    "desk": "Desk",
    "spans": "list[str]",
    "bytes": "str",
})
"""The arguments of `replaced`."""

StartSearchArgs = TypedDict("StartSearchArgs", {
})
"""The arguments of `start_search`."""

StartReplaceArgs = TypedDict("StartReplaceArgs", {
})
"""The arguments of `start_replace`."""

StartGotoArgs = TypedDict("StartGotoArgs", {
})
"""The arguments of `start_goto`."""

ToggleInspectorArgs = TypedDict("ToggleInspectorArgs", {
})
"""The arguments of `toggle_inspector`."""

ClosePanelArgs = TypedDict("ClosePanelArgs", {
})
"""The arguments of `close_panel`."""

SearchArgs = TypedDict("SearchArgs", {
    "query": "str",
})
"""The arguments of `search`."""

RevealArgs = TypedDict("RevealArgs", {
    "index": "int",
})
"""The arguments of `reveal`."""

ReplaceAllArgs = TypedDict("ReplaceAllArgs", {
    "with": "str",
})
"""The arguments of `replace_all`."""

ReplaceSpansArgs = TypedDict("ReplaceSpansArgs", {
    "desk": "Desk",
    "spans": "list[str]",
    "with": "str",
})
"""The arguments of `replace_spans`."""

SearchKindArgs = TypedDict("SearchKindArgs", {
    "i": "int",
})
"""The arguments of `search_kind`."""

SearchCaseArgs = TypedDict("SearchCaseArgs", {
    "on": "bool",
})
"""The arguments of `search_case`."""

SearchWordArgs = TypedDict("SearchWordArgs", {
    "on": "bool",
})
"""The arguments of `search_word`."""

GoToArgs = TypedDict("GoToArgs", {
    "query": "str",
})
"""The arguments of `go_to`."""

SetEndianArgs = TypedDict("SetEndianArgs", {
    "i": "int",
})
"""The arguments of `set_endian`."""

SetModeArgs = TypedDict("SetModeArgs", {
    "i": "int",
})
"""The arguments of `set_mode`."""

SetColumnArgs = TypedDict("SetColumnArgs", {
    "i": "int",
})
"""The arguments of `set_column`."""

DismissArgs = TypedDict("DismissArgs", {
})
"""The arguments of `dismiss`."""

PressedArgs = TypedDict("PressedArgs", {
    "at": "list[str]",
})
"""The arguments of `pressed`."""

ScrolledArgs = TypedDict("ScrolledArgs", {
    "rows": "int",
})
"""The arguments of `scrolled`."""

InsertBytesArgs = TypedDict("InsertBytesArgs", {
    "offset": "int",
    "bytes": "str",
})
"""The arguments of `insert_bytes`."""

OverwriteBytesArgs = TypedDict("OverwriteBytesArgs", {
    "offset": "int",
    "bytes": "str",
})
"""The arguments of `overwrite_bytes`."""

DeleteBytesArgs = TypedDict("DeleteBytesArgs", {
    "offset": "int",
    "len": "int",
})
"""The arguments of `delete_bytes`."""

ForgetArgs = TypedDict("ForgetArgs", {
    "d": "Document",
})
"""The arguments of `forget`."""

class Change(Resource):
    """Change — `tl.so.tilo.hex-editor.Change`."""
    __surface__ = "so.tilo.hex-editor@35422bf2b2064fc9"
    b4ynsjfc: int  # at
    lkghkw33: int  # remove
    n55erru5: str  # insert


class Row(Resource):
    """Row — `tl.so.tilo.hex-editor.Row`."""
    __surface__ = "so.tilo.hex-editor@35422bf2b2064fc9"
    rqqjo5b2: str  # hex
    i23efoqg: str  # changed


class Finding(Resource):
    """Finding — `tl.so.tilo.hex-editor.Finding`."""
    __surface__ = "so.tilo.hex-editor@35422bf2b2064fc9"
    j3rx5nr6: str  # label
    bqtm2sxd: str  # value
    whqswnrc: int  # width


class Span(Resource):
    """Span — `tl.so.tilo.hex-editor.Span`."""
    __surface__ = "so.tilo.hex-editor@35422bf2b2064fc9"
    vjv7m35i: int  # start
    @property
    def _6nu52vd6(self) -> int:
        """end (`6nu52vd6` on the wire)."""
        return self.fields["6nu52vd6"]

    @_6nu52vd6.setter
    def _6nu52vd6(self, value: int) -> None:
        self.edit("6nu52vd6", value=value)


class Desk(Resource):
    """Desk — `tl.so.tilo.hex-editor.Desk`."""
    __surface__ = "so.tilo.hex-editor@35422bf2b2064fc9"
    @property
    def _4pp4s3sf(self) -> int:
        """cursor (`4pp4s3sf` on the wire)."""
        return self.fields["4pp4s3sf"]

    @_4pp4s3sf.setter
    def _4pp4s3sf(self, value: int) -> None:
        self.edit("4pp4s3sf", value=value)
    ta3nqlbq: int | None  # anchor
    d6nzpsjf: str  # column
    mum5d7nq: str  # mode
    xmgji4li: int | None  # high
    b23s2qvq: str  # endian
    ntd56cpd: int  # first
    d4rmfnak: int  # columns
    u54h5izd: list[str]  # rows
    iyf5x4du: list[str]  # changed
    nczhspum: list[str]  # findings
    uswoxemn: list[str]  # hits
    nv3vcmev: int  # at_match
    s777zrl4: bool  # truncated
    gy4cijiy: str  # query
    hfchq6ug: str  # kind
    pzwozqbg: bool  # match_case
    olbisfbn: bool  # whole_word
    jko5an6z: str  # panel
    n7ibv25j: str  # notice
    jslwh26v: str  # notice_kind

    def say(self,
        *,
        words: str,
        kind: str,
    ) -> bool:
        """Say something in the window: a result, or what went wrong."""
        return self.action("say", words=words, kind=kind)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"say"})


class Document(Resource):
    """Document — `tl.so.tilo.hex-editor.Document`."""
    __surface__ = "so.tilo.hex-editor@35422bf2b2064fc9"
    dbkdqzos: str  # name
    teahiasd: int  # opened
    blhbkxjc: str  # base
    wts2l2hg: str  # path
    nqk5afwc: int  # length
    @property
    def _7u75gmtu(self) -> int:
        """edited (`7u75gmtu` on the wire)."""
        return self.fields["7u75gmtu"]

    @_7u75gmtu.setter
    def _7u75gmtu(self, value: int) -> None:
        self.edit("7u75gmtu", value=value)
    @property
    def _3tqozdp3(self) -> list[str]:
        """changes (`3tqozdp3` on the wire)."""
        return self.fields["3tqozdp3"]

    @_3tqozdp3.setter
    def _3tqozdp3(self, value: list[str]) -> None:
        self.edit("3tqozdp3", value=value)
    @property
    def _2fs4ukcj(self) -> list[Entry[Desk]]:
        """desks (`2fs4ukcj` on the wire)."""
        return self.fields["2fs4ukcj"]

    @_2fs4ukcj.setter
    def _2fs4ukcj(self, value: list[Entry[Desk]]) -> None:
        self.edit("2fs4ukcj", value=value)

    def move_cursor(self,
        *,
        desk: Desk,
        at: int,
        extend: bool,
    ) -> bool:
        """The cursor moved to `at`, kept inside the bytes; `extend` keeps where the selection started, and the window's first row follows it when it leaves the rows shown."""
        return self.action("move_cursor", desk=desk, at=at, extend=extend)

    def show(self) -> None:
        """Read the window's rows and the bytes at its cursor from the device again."""
        return self.action("show")

    def show_many(self, items: list[ShowArgs]) -> list[dict[str, Any]]:
        """Runs `show` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show", items)

    # the app's `edit` action is the SDK's own: run it with `action("edit")`.
    def save(self) -> None:
        """Saves the buffer where it came from, or asks where when it stands nowhere yet."""
        return self.action("save")

    def save_many(self, items: list[SaveArgs]) -> list[dict[str, Any]]:
        """Runs `save` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("save", items)

    def save_bytes(self) -> Any:
        """The bytes as a file the person places with the system's save chooser, which the window shows."""
        return self.action("save_bytes")

    def rebased(self,
        *,
        address: str,
        length: int,
    ) -> None:
        """What was saved is the base: no edit stands against it any more."""
        return self.action("rebased", address=address, length=length)

    def rebased_many(self, items: list[RebasedArgs]) -> list[dict[str, Any]]:
        """Runs `rebased` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rebased", items)

    def press(self, *, key: str) -> None:
        """The keys pressed on the grid, as the person's keyboard names them."""
        return self.action("press", key=key)

    def chord(self,
        *,
        desk: Desk,
        key: str,
    ) -> None:
        """The Ctrl chords typed while the bytes have the keyboard."""
        return self.action("chord", desk=desk, key=key)

    def moved(self,
        *,
        desk: Desk,
        at: int,
        extend: bool,
    ) -> None:
        """The cursor moved to `at`: the rows and the inspector read there."""
        return self.action("moved", desk=desk, at=at, extend=extend)

    def moved_many(self, items: list[MovedArgs]) -> list[dict[str, Any]]:
        """Runs `moved` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("moved", items)

    def type_key(self,
        *,
        desk: Desk,
        one: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("type_key", desk=desk, one=one)

    def type_key_many(self, items: list[TypeKeyArgs]) -> list[dict[str, Any]]:
        """Runs `type_key` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("type_key", items)

    def type_hex(self,
        *,
        desk: Desk,
        value: int,
    ) -> None:
        """Type one hexadecimal digit: in overwrite mode the first sets the high nibble and the second completes the byte; in insert mode each completed byte is spliced in."""
        return self.action("type_hex", desk=desk, value=value)

    def type_hex_many(self, items: list[TypeHexArgs]) -> list[dict[str, Any]]:
        """Runs `type_hex` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("type_hex", items)

    def type_ascii(self,
        *,
        desk: Desk,
        one: str,
    ) -> None:
        """Type a character into the ASCII column, then advance."""
        return self.action("type_ascii", desk=desk, one=one)

    def type_ascii_many(self, items: list[TypeAsciiArgs]) -> list[dict[str, Any]]:
        """Runs `type_ascii` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("type_ascii", items)

    def backspace(self, *, desk: Desk) -> None:
        """In insert mode the byte before the cursor goes; in overwrite mode the cursor only steps back."""
        return self.action("backspace", desk=desk)

    def backspace_many(self, items: list[BackspaceArgs]) -> list[dict[str, Any]]:
        """Runs `backspace` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("backspace", items)

    def delete_at_cursor(self, *, desk: Desk) -> None:
        """The selection goes, or else the byte under the cursor."""
        return self.action("delete_at_cursor", desk=desk)

    def delete_at_cursor_many(self, items: list[DeleteAtCursorArgs]) -> list[dict[str, Any]]:
        """Runs `delete_at_cursor` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_at_cursor", items)

    def select_all(self, *, desk: Desk) -> None:
        """Runs the app's action."""
        return self.action("select_all", desk=desk)

    def select_all_many(self, items: list[SelectAllArgs]) -> list[dict[str, Any]]:
        """Runs `select_all` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("select_all", items)

    def copy(self) -> None:
        """Copies the selected bytes in the column's form: `50 4e 47 44`, or `PNGD`."""
        return self.action("copy")

    def cut(self) -> None:
        """Copies the selection and deletes it."""
        return self.action("cut")

    def paste(self) -> None:
        """Pastes the clipboard at the selection, or at the cursor, in the focus mode."""
        return self.action("paste")

    def paste_many(self, items: list[PasteArgs]) -> list[dict[str, Any]]:
        """Runs `paste` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("paste", items)

    def replaced(self,
        *,
        desk: Desk,
        spans: list[str],
        bytes: str,
    ) -> None:
        """Replaces each span with the same bytes, as one step."""
        return self.action("replaced", desk=desk, spans=spans, bytes=bytes)

    def replaced_many(self, items: list[ReplacedArgs]) -> list[dict[str, Any]]:
        """Runs `replaced` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("replaced", items)

    def start_search(self) -> None:
        """Opens the search strip."""
        return self.action("start_search")

    def start_search_many(self, items: list[StartSearchArgs]) -> list[dict[str, Any]]:
        """Runs `start_search` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("start_search", items)

    def start_replace(self) -> None:
        """Opens the replace strip."""
        return self.action("start_replace")

    def start_replace_many(self, items: list[StartReplaceArgs]) -> list[dict[str, Any]]:
        """Runs `start_replace` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("start_replace", items)

    def start_goto(self) -> None:
        """Opens the go-to-offset strip."""
        return self.action("start_goto")

    def start_goto_many(self, items: list[StartGotoArgs]) -> list[dict[str, Any]]:
        """Runs `start_goto` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("start_goto", items)

    def toggle_inspector(self) -> None:
        """Shows or hides the inspector."""
        return self.action("toggle_inspector")

    def toggle_inspector_many(self, items: list[ToggleInspectorArgs]) -> list[dict[str, Any]]:
        """Runs `toggle_inspector` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("toggle_inspector", items)

    def close_panel(self) -> None:
        """Closes the strip under the bar."""
        return self.action("close_panel")

    def close_panel_many(self, items: list[ClosePanelArgs]) -> list[dict[str, Any]]:
        """Runs `close_panel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("close_panel", items)

    def search(self, *, query: str) -> None:
        """Finds `query` in the bytes: a hexadecimal pattern, or text under its rules."""
        return self.action("search", query=query)

    def search_many(self, items: list[SearchArgs]) -> list[dict[str, Any]]:
        """Runs `search` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("search", items)

    def reveal(self, *, index: int) -> None:
        """Puts the cursor on the match at `index`."""
        return self.action("reveal", index=index)

    def reveal_many(self, items: list[RevealArgs]) -> list[dict[str, Any]]:
        """Runs `reveal` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("reveal", items)

    def replace_all(self, *, with_: str) -> None:
        """Replaces every match the search found with `with`."""
        args: dict[str, Any] = {}
        args["with"] = with_
        return self.action("replace_all", **args)

    def replace_all_many(self, items: list[ReplaceAllArgs]) -> list[dict[str, Any]]:
        """Runs `replace_all` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("replace_all", items)

    def replace_spans(self,
        *,
        desk: Desk,
        spans: list[str],
        with_: str,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["desk"] = desk
        args["spans"] = spans
        args["with"] = with_
        return self.action("replace_spans", **args)

    def replace_spans_many(self, items: list[ReplaceSpansArgs]) -> list[dict[str, Any]]:
        """Runs `replace_spans` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("replace_spans", items)

    def search_kind(self, *, i: int) -> None:
        """Reads the search strip's choices: text or hexadecimal, letter case and whole words."""
        return self.action("search_kind", i=i)

    def search_kind_many(self, items: list[SearchKindArgs]) -> list[dict[str, Any]]:
        """Runs `search_kind` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("search_kind", items)

    def search_case(self, *, on: bool) -> None:
        """Runs the app's action."""
        return self.action("search_case", on=on)

    def search_case_many(self, items: list[SearchCaseArgs]) -> list[dict[str, Any]]:
        """Runs `search_case` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("search_case", items)

    def search_word(self, *, on: bool) -> None:
        """Runs the app's action."""
        return self.action("search_word", on=on)

    def search_word_many(self, items: list[SearchWordArgs]) -> list[dict[str, Any]]:
        """Runs `search_word` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("search_word", items)

    def go_to(self, *, query: str) -> None:
        """Moves the cursor to the offset `query` names."""
        return self.action("go_to", query=query)

    def go_to_many(self, items: list[GoToArgs]) -> list[dict[str, Any]]:
        """Runs `go_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("go_to", items)

    def set_endian(self, *, i: int) -> None:
        """Runs the app's action."""
        return self.action("set_endian", i=i)

    def set_endian_many(self, items: list[SetEndianArgs]) -> list[dict[str, Any]]:
        """Runs `set_endian` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_endian", items)

    def set_mode(self, *, i: int) -> None:
        """Runs the app's action."""
        return self.action("set_mode", i=i)

    def set_mode_many(self, items: list[SetModeArgs]) -> list[dict[str, Any]]:
        """Runs `set_mode` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_mode", items)

    def set_column(self, *, i: int) -> None:
        """Runs the app's action."""
        return self.action("set_column", i=i)

    def set_column_many(self, items: list[SetColumnArgs]) -> list[dict[str, Any]]:
        """Runs `set_column` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_column", items)

    def dismiss(self) -> None:
        """Runs the app's action."""
        return self.action("dismiss")

    def dismiss_many(self, items: list[DismissArgs]) -> list[dict[str, Any]]:
        """Runs `dismiss` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("dismiss", items)

    def pressed(self, *, at: list[str]) -> None:
        """A press on the grid: the byte at `at` gets the cursor, the column pressed the focus, and `'shift'` keeps where the selection started."""
        return self.action("pressed", at=at)

    def pressed_many(self, items: list[PressedArgs]) -> list[dict[str, Any]]:
        """Runs `pressed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("pressed", items)

    def scrolled(self, *, rows: int) -> None:
        """The wheel or the bar moved the window by `rows`."""
        return self.action("scrolled", rows=rows)

    def scrolled_many(self, items: list[ScrolledArgs]) -> list[dict[str, Any]]:
        """Runs `scrolled` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("scrolled", items)

    def insert_bytes(self,
        *,
        offset: int,
        bytes: str,
    ) -> None:
        """Inserts `bytes` (hexadecimal, like `de ad be ef`) at `offset`."""
        return self.action("insert_bytes", offset=offset, bytes=bytes)

    def insert_bytes_many(self, items: list[InsertBytesArgs]) -> list[dict[str, Any]]:
        """Runs `insert_bytes` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("insert_bytes", items)

    def overwrite_bytes(self,
        *,
        offset: int,
        bytes: str,
    ) -> None:
        """Replaces the bytes at `offset` with `bytes` (hexadecimal), never growing the file."""
        return self.action("overwrite_bytes", offset=offset, bytes=bytes)

    def overwrite_bytes_many(self, items: list[OverwriteBytesArgs]) -> list[dict[str, Any]]:
        """Runs `overwrite_bytes` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("overwrite_bytes", items)

    def delete_bytes(self,
        *,
        offset: int,
        len: int,
    ) -> None:
        """Removes `len` bytes at `offset`."""
        return self.action("delete_bytes", offset=offset, len=len)

    def delete_bytes_many(self, items: list[DeleteBytesArgs]) -> list[dict[str, Any]]:
        """Runs `delete_bytes` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_bytes", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"move_cursor", "show", "show_many", "save", "save_many", "save_bytes", "rebased", "rebased_many", "press", "chord", "moved", "moved_many", "type_key", "type_key_many", "type_hex", "type_hex_many", "type_ascii", "type_ascii_many", "backspace", "backspace_many", "delete_at_cursor", "delete_at_cursor_many", "select_all", "select_all_many", "copy", "cut", "paste", "paste_many", "replaced", "replaced_many", "start_search", "start_search_many", "start_replace", "start_replace_many", "start_goto", "start_goto_many", "toggle_inspector", "toggle_inspector_many", "close_panel", "close_panel_many", "search", "search_many", "reveal", "reveal_many", "replace_all", "replace_all_many", "replace_spans", "replace_spans_many", "search_kind", "search_kind_many", "search_case", "search_case_many", "search_word", "search_word_many", "go_to", "go_to_many", "set_endian", "set_endian_many", "set_mode", "set_mode_many", "set_column", "set_column_many", "dismiss", "dismiss_many", "pressed", "pressed_many", "scrolled", "scrolled_many", "insert_bytes", "insert_bytes_many", "overwrite_bytes", "overwrite_bytes_many", "delete_bytes", "delete_bytes_many"})


class Library(Resource):
    """Library — `tl.so.tilo.hex-editor.Library`."""
    __surface__ = "so.tilo.hex-editor@35422bf2b2064fc9"
    fu57pivb: list[Entry[Document]]  # documents

    def open_path(self, *, path: str) -> Document:
        """Opens the file at `path` on this device as its own buffer."""
        return self.action("open_path", path=path)

    def refused_buffer(self,
        *,
        path: str,
        why: str,
    ) -> Document:
        """Runs the app's action."""
        return self.action("refused_buffer", path=path, why=why)

    def open_file(self, *, file: Any) -> Document:
        """Opens a file a person chose, or one another app handed over, as its own buffer. Its bytes are the address the content store gave them, so every device reads the same file."""
        return self.action("open_file", file=file)

    def opened_buffer(self,
        *,
        name: str,
        address: str,
        path: str,
        length: int,
    ) -> Document:
        """The buffer of the bytes at `address`: the one already open on them, read afresh, or a new one."""
        return self.action("opened_buffer", name=name, address=address, path=path, length=length)

    def new_buffer(self) -> Document:
        """An empty buffer to type bytes into, saved wherever the person says."""
        return self.action("new_buffer")

    def empty_buffer(self, *, name: str) -> Document:
        """Runs the app's action."""
        return self.action("empty_buffer", name=name)

    def open_chosen(self, *, file: Any) -> Document:
        """The action the person's file chooser fills."""
        return self.action("open_chosen", file=file)

    def forget(self, *, d: Document) -> None:
        """Forgets a buffer, its edits with it."""
        return self.action("forget", d=d)

    def forget_many(self, items: list[ForgetArgs]) -> list[dict[str, Any]]:
        """Runs `forget` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("forget", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"open_path", "refused_buffer", "open_file", "opened_buffer", "new_buffer", "empty_buffer", "open_chosen", "forget", "forget_many"})


class OpenAsk(Resource):
    """OpenAsk — `tl.so.tilo.hex-editor.OpenAsk`."""
    __surface__ = "so.tilo.hex-editor@35422bf2b2064fc9"
    rruucmz4: str  # path


class OpenedAnswer(Resource):
    """OpenedAnswer — `tl.so.tilo.hex-editor.OpenedAnswer`."""
    __surface__ = "so.tilo.hex-editor@35422bf2b2064fc9"
    cevle23p: str  # name
    idpg6pxk: str  # address
    cr3tncji: int  # length


class RowsAsk(Resource):
    """RowsAsk — `tl.so.tilo.hex-editor.RowsAsk`."""
    __surface__ = "so.tilo.hex-editor@35422bf2b2064fc9"
    ldq6mtad: str  # address
    @property
    def _2t4i36rg(self) -> list[str]:
        """changes (`2t4i36rg` on the wire)."""
        return self.fields["2t4i36rg"]

    @_2t4i36rg.setter
    def _2t4i36rg(self, value: list[str]) -> None:
        self.edit("2t4i36rg", value=value)
    @property
    def _52p24f3b(self) -> int:
        """first (`52p24f3b` on the wire)."""
        return self.fields["52p24f3b"]

    @_52p24f3b.setter
    def _52p24f3b(self, value: int) -> None:
        self.edit("52p24f3b", value=value)
    j3ztka6c: int  # count
    jmwhucjc: int  # columns
    tjv2yhp3: str  # endian
    oowz47v3: int  # cursor


class RowsAnswer(Resource):
    """RowsAnswer — `tl.so.tilo.hex-editor.RowsAnswer`."""
    __surface__ = "so.tilo.hex-editor@35422bf2b2064fc9"
    yjjtzhyt: int  # length
    ao74hr7t: int  # edited
    lgghdfh2: bool  # read
    df32ywi4: str  # error
    jhbkwvr2: list[str]  # rows
    @property
    def _3ugkwm42(self) -> list[str]:
        """changed (`3ugkwm42` on the wire)."""
        return self.fields["3ugkwm42"]

    @_3ugkwm42.setter
    def _3ugkwm42(self, value: list[str]) -> None:
        self.edit("3ugkwm42", value=value)
    xhxdxsgb: list[str]  # findings


class FindAsk(Resource):
    """FindAsk — `tl.so.tilo.hex-editor.FindAsk`."""
    __surface__ = "so.tilo.hex-editor@35422bf2b2064fc9"
    @property
    def _6grc7o75(self) -> str:
        """address (`6grc7o75` on the wire)."""
        return self.fields["6grc7o75"]

    @_6grc7o75.setter
    def _6grc7o75(self, value: str) -> None:
        self.edit("6grc7o75", value=value)
    @property
    def _5yumynmf(self) -> list[str]:
        """changes (`5yumynmf` on the wire)."""
        return self.fields["5yumynmf"]

    @_5yumynmf.setter
    def _5yumynmf(self, value: list[str]) -> None:
        self.edit("5yumynmf", value=value)
    ljornfmm: str  # query
    wyvxlbd4: str  # kind
    b4bt6mtj: bool  # case
    bxix7im4: bool  # word
    qyar3ymw: int  # limit


class FoundAnswer(Resource):
    """FoundAnswer — `tl.so.tilo.hex-editor.FoundAnswer`."""
    __surface__ = "so.tilo.hex-editor@35422bf2b2064fc9"
    t6d5sfoz: list[str]  # hits
    @property
    def _4fdr6kla(self) -> bool:
        """truncated (`4fdr6kla` on the wire)."""
        return self.fields["4fdr6kla"]

    @_4fdr6kla.setter
    def _4fdr6kla(self, value: bool) -> None:
        self.edit("4fdr6kla", value=value)


class ApplyAsk(Resource):
    """ApplyAsk — `tl.so.tilo.hex-editor.ApplyAsk`."""
    __surface__ = "so.tilo.hex-editor@35422bf2b2064fc9"
    ba424tqt: str  # address
    qtf35usu: list[str]  # changes
    r5xf3t6m: str  # op
    z2ahkv3u: int  # at
    @property
    def _3kwri4q5(self) -> int:
        """len (`3kwri4q5` on the wire)."""
        return self.fields["3kwri4q5"]

    @_3kwri4q5.setter
    def _3kwri4q5(self, value: int) -> None:
        self.edit("3kwri4q5", value=value)
    pk7jqula: str  # bytes


class AppliedAnswer(Resource):
    """AppliedAnswer — `tl.so.tilo.hex-editor.AppliedAnswer`."""
    __surface__ = "so.tilo.hex-editor@35422bf2b2064fc9"
    @property
    def _2w7szbth(self) -> str:
        """change (`2w7szbth` on the wire)."""
        return self.fields["2w7szbth"]

    @_2w7szbth.setter
    def _2w7szbth(self, value: str) -> None:
        self.edit("2w7szbth", value=value)
    ll4ibsb5: int  # length


class ReplaceAsk(Resource):
    """ReplaceAsk — `tl.so.tilo.hex-editor.ReplaceAsk`."""
    __surface__ = "so.tilo.hex-editor@35422bf2b2064fc9"
    @property
    def _3riih5ln(self) -> str:
        """address (`3riih5ln` on the wire)."""
        return self.fields["3riih5ln"]

    @_3riih5ln.setter
    def _3riih5ln(self, value: str) -> None:
        self.edit("3riih5ln", value=value)
    o4kbz7cm: list[str]  # changes
    ptnk64f6: list[str]  # spans
    us73s4xt: str  # bytes
    mj26u2qk: str  # mode


class ReplacedAnswer(Resource):
    """ReplacedAnswer — `tl.so.tilo.hex-editor.ReplacedAnswer`."""
    __surface__ = "so.tilo.hex-editor@35422bf2b2064fc9"
    j3lt4zli: list[str]  # changes
    @property
    def _3kaouk56(self) -> int:
        """count (`3kaouk56` on the wire)."""
        return self.fields["3kaouk56"]

    @_3kaouk56.setter
    def _3kaouk56(self, value: int) -> None:
        self.edit("3kaouk56", value=value)
    @property
    def _4fce7y46(self) -> int:
        """length (`4fce7y46` on the wire)."""
        return self.fields["4fce7y46"]

    @_4fce7y46.setter
    def _4fce7y46(self, value: int) -> None:
        self.edit("4fce7y46", value=value)


class RangeAsk(Resource):
    """RangeAsk — `tl.so.tilo.hex-editor.RangeAsk`."""
    __surface__ = "so.tilo.hex-editor@35422bf2b2064fc9"
    @property
    def _3surtarm(self) -> str:
        """address (`3surtarm` on the wire)."""
        return self.fields["3surtarm"]

    @_3surtarm.setter
    def _3surtarm(self, value: str) -> None:
        self.edit("3surtarm", value=value)
    l7dzgaqe: list[str]  # changes
    @property
    def _3v66hb5v(self) -> int:
        """start (`3v66hb5v` on the wire)."""
        return self.fields["3v66hb5v"]

    @_3v66hb5v.setter
    def _3v66hb5v(self, value: int) -> None:
        self.edit("3v66hb5v", value=value)
    fsjcytnx: int  # end
    fyp52l2r: str  # kind


class RangeAnswer(Resource):
    """RangeAnswer — `tl.so.tilo.hex-editor.RangeAnswer`."""
    __surface__ = "so.tilo.hex-editor@35422bf2b2064fc9"
    d7nxzpqz: str  # text


class BytesAsk(Resource):
    """BytesAsk — `tl.so.tilo.hex-editor.BytesAsk`."""
    __surface__ = "so.tilo.hex-editor@35422bf2b2064fc9"
    @property
    def _66cpqgom(self) -> str:
        """address (`66cpqgom` on the wire)."""
        return self.fields["66cpqgom"]

    @_66cpqgom.setter
    def _66cpqgom(self, value: str) -> None:
        self.edit("66cpqgom", value=value)
    @property
    def _4tyyvxqh(self) -> list[str]:
        """changes (`4tyyvxqh` on the wire)."""
        return self.fields["4tyyvxqh"]

    @_4tyyvxqh.setter
    def _4tyyvxqh(self, value: list[str]) -> None:
        self.edit("4tyyvxqh", value=value)


class BytesAnswer(Resource):
    """BytesAnswer — `tl.so.tilo.hex-editor.BytesAnswer`."""
    __surface__ = "so.tilo.hex-editor@35422bf2b2064fc9"
    smxqtshm: str  # bytes


class SaveAsk(Resource):
    """SaveAsk — `tl.so.tilo.hex-editor.SaveAsk`."""
    __surface__ = "so.tilo.hex-editor@35422bf2b2064fc9"
    @property
    def _62acy6x2(self) -> str:
        """address (`62acy6x2` on the wire)."""
        return self.fields["62acy6x2"]

    @_62acy6x2.setter
    def _62acy6x2(self, value: str) -> None:
        self.edit("62acy6x2", value=value)
    @property
    def _2d44lw44(self) -> list[str]:
        """changes (`2d44lw44` on the wire)."""
        return self.fields["2d44lw44"]

    @_2d44lw44.setter
    def _2d44lw44(self, value: list[str]) -> None:
        self.edit("2d44lw44", value=value)
    sujs7lc2: str  # path


class SavedAnswer(Resource):
    """SavedAnswer — `tl.so.tilo.hex-editor.SavedAnswer`."""
    __surface__ = "so.tilo.hex-editor@35422bf2b2064fc9"
    fnprm7qy: str  # address
    bnwrfq64: int  # length


class HexEditor(Resource):
    """Hex Editor — `tl.so.tilo.hex-editor`."""
    __surface__ = "so.tilo.hex-editor@35422bf2b2064fc9"

