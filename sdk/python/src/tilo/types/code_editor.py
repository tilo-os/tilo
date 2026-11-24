"""Types for Code Editor (`so.tilo.code-editor`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Entry` is the app's `tl.so.tilo.code-editor.Entry` at `code-editor/<resource>`, read with
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
    "EntryKind",
    "Severity",
    "ItemKind",
    "Entry",
    "Problem",
    "Found",
    "Item",
    "Command",
    "Hit",
    "Completion",
    "Location",
    "Loaded",
    "Stamped",
    "Exists",
    "DiskAsk",
    "DiskSave",
    "DiskEntry",
    "Listing",
    "SearchAsk",
    "RunAsk",
    "JobAnswer",
    "SearchState",
    "RunState",
    "ServerAsk",
    "DocumentAsk",
    "AskAsk",
    "Progress",
    "Diag",
    "CompletionAnswer",
    "HoverAnswer",
    "DefinitionAnswer",
    "FormatEdit",
    "FormatAnswer",
    "ServerProgress",
    "ServerState",
    "Project",
    "CodeEditor",
    "SetLanguageArgs",
    "ReplaceTextArgs",
    "ReplaceRangeArgs",
    "DuplicateLineArgs",
    "MoveLineArgs",
    "DeleteLineArgs",
    "ToggleCommentArgs",
    "IndentLineArgs",
    "OutdentLineArgs",
    "RevealLineArgs",
    "AddChildArgs",
    "RenameArgs",
    "MoveEntryArgs",
    "MoveAfterArgs",
    "DeleteArgs",
    "FindInFileArgs",
    "CheckArgs",
    "OpenFolderArgs",
    "ReadIntoArgs",
    "ReadFolderIntoArgs",
    "ReadTextArgs",
    "OpenFileArgs",
    "SaveArgs",
    "OverwriteArgs",
    "ReloadArgs",
    "SaveAllArgs",
    "SaveIntoArgs",
    "MoveOnDiskArgs",
    "ImportDroppedArgs",
    "RunArgs",
    "StopArgs",
    "RerunArgs",
    "RerunTestArgs",
    "ProjectRunArgs",
    "ClearCommandsArgs",
    "OpenTerminalArgs",
    "PollArgs",
    "RevealItemArgs",
    "GoToLineArgs",
    "SearchProjectArgs",
    "StopSearchArgs",
    "OpenHitArgs",
    "EnsureServerArgs",
    "PollServerArgs",
    "ReadAnswerArgs",
    "CompleteArgs",
    "HoverArgs",
    "DefinitionArgs",
    "AcceptCompletionArgs",
    "FormatFileArgs",
    "ToggleLanguageServersArgs",
    "ToggleFormatOnSaveArgs",
    "RestartServerArgs",
]

EntryKind = Literal["file", "folder"]
"""What an entry of the project is: code that is written, or a place that holds entries."""

Severity = Literal["error", "warning", "info", "hint"]
"""How much a problem matters, as the editor shows it."""

ItemKind = Literal["error", "warning", "location", "test_passed", "test_failed", "test_ignored", "change"]
"""What a line of a command's output is: a problem a tool found, a place it named, a test, or a change."""

SetLanguageArgs = TypedDict("SetLanguageArgs", {
    "language": "str",
})
"""The arguments of `set_language`."""

ReplaceTextArgs = TypedDict("ReplaceTextArgs", {
    "text": "str",
})
"""The arguments of `replace_text`."""

ReplaceRangeArgs = TypedDict("ReplaceRangeArgs", {
    "at": "int",
    "count": NotRequired["int | None"],
    "text": "str",
})
"""The arguments of `replace_range`."""

DuplicateLineArgs = TypedDict("DuplicateLineArgs", {
    "line": NotRequired["int | None"],
})
"""The arguments of `duplicate_line`."""

MoveLineArgs = TypedDict("MoveLineArgs", {
    "line": NotRequired["int | None"],
    "up": NotRequired["bool | None"],
})
"""The arguments of `move_line`."""

DeleteLineArgs = TypedDict("DeleteLineArgs", {
    "line": NotRequired["int | None"],
})
"""The arguments of `delete_line`."""

ToggleCommentArgs = TypedDict("ToggleCommentArgs", {
    "line": NotRequired["int | None"],
})
"""The arguments of `toggle_comment`."""

IndentLineArgs = TypedDict("IndentLineArgs", {
    "line": NotRequired["int | None"],
})
"""The arguments of `indent_line`."""

OutdentLineArgs = TypedDict("OutdentLineArgs", {
    "line": NotRequired["int | None"],
})
"""The arguments of `outdent_line`."""

RevealLineArgs = TypedDict("RevealLineArgs", {
    "line": NotRequired["int | None"],
    "column": NotRequired["int | None"],
})
"""The arguments of `reveal_line`."""

AddChildArgs = TypedDict("AddChildArgs", {
})
"""The arguments of `add_child`."""

RenameArgs = TypedDict("RenameArgs", {
    "entry": "Entry",
    "name": "str",
})
"""The arguments of `rename`."""

MoveEntryArgs = TypedDict("MoveEntryArgs", {
    "entry": "Entry",
    "folder": NotRequired["Entry | None"],
    "before": NotRequired["Entry | None"],
})
"""The arguments of `move_entry`."""

MoveAfterArgs = TypedDict("MoveAfterArgs", {
    "entry": "Entry",
    "after": "Entry",
})
"""The arguments of `move_after`."""

DeleteArgs = TypedDict("DeleteArgs", {
    "entry": "Entry",
})
"""The arguments of `delete`."""

FindInFileArgs = TypedDict("FindInFileArgs", {
    "entry": "Entry",
    "text": "str",
    "case": NotRequired["bool | None"],
    "word": NotRequired["bool | None"],
})
"""The arguments of `find_in_file`."""

CheckArgs = TypedDict("CheckArgs", {
    "entry": "Entry",
})
"""The arguments of `check`."""

OpenFolderArgs = TypedDict("OpenFolderArgs", {
    "folder": "str",
})
"""The arguments of `open_folder`."""

ReadIntoArgs = TypedDict("ReadIntoArgs", {
    "parent": NotRequired["Entry | None"],
    "answer": "str",
})
"""The arguments of `read_into`."""

ReadFolderIntoArgs = TypedDict("ReadFolderIntoArgs", {
    "folder": "Entry",
    "path": "str",
})
"""The arguments of `read_folder_into`."""

ReadTextArgs = TypedDict("ReadTextArgs", {
    "entry": "Entry",
})
"""The arguments of `read_text`."""

OpenFileArgs = TypedDict("OpenFileArgs", {
    "file": "Any",
})
"""The arguments of `open_file`."""

SaveArgs = TypedDict("SaveArgs", {
    "entry": "Entry",
})
"""The arguments of `save`."""

OverwriteArgs = TypedDict("OverwriteArgs", {
    "entry": "Entry",
})
"""The arguments of `overwrite`."""

ReloadArgs = TypedDict("ReloadArgs", {
    "entry": "Entry",
})
"""The arguments of `reload`."""

SaveAllArgs = TypedDict("SaveAllArgs", {
})
"""The arguments of `save_all`."""

SaveIntoArgs = TypedDict("SaveIntoArgs", {
    "entry": "Entry",
    "files": "list[Any]",
})
"""The arguments of `save_into`."""

MoveOnDiskArgs = TypedDict("MoveOnDiskArgs", {
    "entry": "Entry",
    "to": "str",
})
"""The arguments of `move_on_disk`."""

ImportDroppedArgs = TypedDict("ImportDroppedArgs", {
    "files": "list[Any]",
})
"""The arguments of `import_dropped`."""

RunArgs = TypedDict("RunArgs", {
    "command": "str",
    "cwd": NotRequired["str | None"],
})
"""The arguments of `run`."""

StopArgs = TypedDict("StopArgs", {
})
"""The arguments of `stop`."""

RerunArgs = TypedDict("RerunArgs", {
    "run": "Command",
})
"""The arguments of `rerun`."""

RerunTestArgs = TypedDict("RerunTestArgs", {
    "run": "Command",
    "test": "str",
})
"""The arguments of `rerun_test`."""

ProjectRunArgs = TypedDict("ProjectRunArgs", {
    "cwd": "str",
    "command": "str",
})
"""The arguments of `run_`."""

ClearCommandsArgs = TypedDict("ClearCommandsArgs", {
})
"""The arguments of `clear_commands`."""

OpenTerminalArgs = TypedDict("OpenTerminalArgs", {
})
"""The arguments of `open_terminal`."""

PollArgs = TypedDict("PollArgs", {
})
"""The arguments of `poll`."""

RevealItemArgs = TypedDict("RevealItemArgs", {
    "path": "str",
    "line": NotRequired["int | None"],
})
"""The arguments of `reveal_item`."""

GoToLineArgs = TypedDict("GoToLineArgs", {
    "path": "str",
    "line": NotRequired["int | None"],
})
"""The arguments of `go_to_line`."""

SearchProjectArgs = TypedDict("SearchProjectArgs", {
    "needle": "str",
    "case": NotRequired["bool | None"],
    "word": NotRequired["bool | None"],
})
"""The arguments of `search_project`."""

StopSearchArgs = TypedDict("StopSearchArgs", {
})
"""The arguments of `stop_search`."""

OpenHitArgs = TypedDict("OpenHitArgs", {
    "path": "str",
    "line": NotRequired["int | None"],
})
"""The arguments of `open_hit`."""

EnsureServerArgs = TypedDict("EnsureServerArgs", {
    "entry": "Entry",
})
"""The arguments of `ensure_server`."""

PollServerArgs = TypedDict("PollServerArgs", {
})
"""The arguments of `poll_server`."""

ReadAnswerArgs = TypedDict("ReadAnswerArgs", {
    "answer": "str",
})
"""The arguments of `read_answer`."""

CompleteArgs = TypedDict("CompleteArgs", {
    "entry": "Entry",
    "line": NotRequired["int | None"],
    "column": NotRequired["int | None"],
})
"""The arguments of `complete`."""

HoverArgs = TypedDict("HoverArgs", {
    "entry": "Entry",
    "line": NotRequired["int | None"],
    "column": NotRequired["int | None"],
})
"""The arguments of `hover`."""

DefinitionArgs = TypedDict("DefinitionArgs", {
    "entry": "Entry",
    "line": NotRequired["int | None"],
    "column": NotRequired["int | None"],
})
"""The arguments of `definition`."""

AcceptCompletionArgs = TypedDict("AcceptCompletionArgs", {
    "entry": "Entry",
    "label": "str",
    "insert": "str",
})
"""The arguments of `accept_completion`."""

FormatFileArgs = TypedDict("FormatFileArgs", {
    "entry": "Entry",
})
"""The arguments of `format_file`."""

ToggleLanguageServersArgs = TypedDict("ToggleLanguageServersArgs", {
    "servers": NotRequired["bool | None"],
})
"""The arguments of `toggle_language_servers`."""

ToggleFormatOnSaveArgs = TypedDict("ToggleFormatOnSaveArgs", {
    "format": NotRequired["bool | None"],
})
"""The arguments of `toggle_format_on_save`."""

RestartServerArgs = TypedDict("RestartServerArgs", {
})
"""The arguments of `restart_server`."""

class Entry(Resource):
    """Entry — `tl.so.tilo.code-editor.Entry`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    jr3jrrmx: EntryKind  # kind
    wlnikenq: str  # name
    otwgp44c: Entry | None  # parent
    fav4hm4j: str  # language
    zbluopai: str  # text
    n6s6du45: str  # disk
    lkyxb2f7: int  # stamp
    wxaqrw5b: int  # stamp_len
    bvn4nnf5: str  # saved
    uhopusbk: bool  # stale
    o3gbo56k: list[Problem]  # problems
    jwc6eef6: str  # checked
    jwkhracm: str  # source
    zu6g3bmd: int  # reveal_line
    k4etx7ch: int  # reveal_column
    wtzldj2n: int  # reveal_ask

    def set_language(self, *, language: str) -> None:
        """Sets the language `entry` is coloured and checked as, by the name of a language the editor knows."""
        return self.action("set_language", language=language)

    def set_language_many(self, items: list[SetLanguageArgs]) -> list[dict[str, Any]]:
        """Runs `set_language` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_language", items)

    def replace_text(self, *, text: str) -> None:
        """Writes `text` as the whole file, for an agent or a script that holds a file's new text."""
        return self.action("replace_text", text=text)

    def replace_text_many(self, items: list[ReplaceTextArgs]) -> list[dict[str, Any]]:
        """Runs `replace_text` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("replace_text", items)

    def replace_range(self,
        *,
        at: int,
        text: str,
        count: int | None = None,
    ) -> None:
        """Replaces `count` characters from `at` with `text`: a range or a caret in one file, counted in characters from its start."""
        args: dict[str, Any] = {}
        args["at"] = at
        if count is not None:
            args["count"] = count
        args["text"] = text
        return self.action("replace_range", **args)

    def replace_range_many(self, items: list[ReplaceRangeArgs]) -> list[dict[str, Any]]:
        """Runs `replace_range` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("replace_range", items)

    def duplicate_line(self,
        *,
        line: int | None = None,
    ) -> None:
        """Copies a line of the file to the line under it."""
        args: dict[str, Any] = {}
        if line is not None:
            args["line"] = line
        return self.action("duplicate_line", **args)

    def duplicate_line_many(self, items: list[DuplicateLineArgs]) -> list[dict[str, Any]]:
        """Runs `duplicate_line` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("duplicate_line", items)

    def move_line(self,
        *,
        line: int | None = None,
        up: bool | None = None,
    ) -> None:
        """Moves a line over the one above it (`up`) or under the one below it."""
        args: dict[str, Any] = {}
        if line is not None:
            args["line"] = line
        if up is not None:
            args["up"] = up
        return self.action("move_line", **args)

    def move_line_many(self, items: list[MoveLineArgs]) -> list[dict[str, Any]]:
        """Runs `move_line` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_line", items)

    def delete_line(self,
        *,
        line: int | None = None,
    ) -> None:
        """Deletes a line of the file."""
        args: dict[str, Any] = {}
        if line is not None:
            args["line"] = line
        return self.action("delete_line", **args)

    def delete_line_many(self, items: list[DeleteLineArgs]) -> list[dict[str, Any]]:
        """Runs `delete_line` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_line", items)

    def toggle_comment(self,
        *,
        line: int | None = None,
    ) -> None:
        """Comments out a line, or takes its comment away when the line stands commented already: the marker is the language's own, at the line's own indentation."""
        args: dict[str, Any] = {}
        if line is not None:
            args["line"] = line
        return self.action("toggle_comment", **args)

    def toggle_comment_many(self, items: list[ToggleCommentArgs]) -> list[dict[str, Any]]:
        """Runs `toggle_comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("toggle_comment", items)

    def indent_line(self,
        *,
        line: int | None = None,
    ) -> None:
        """Adds one indentation step to a line."""
        args: dict[str, Any] = {}
        if line is not None:
            args["line"] = line
        return self.action("indent_line", **args)

    def indent_line_many(self, items: list[IndentLineArgs]) -> list[dict[str, Any]]:
        """Runs `indent_line` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("indent_line", items)

    def outdent_line(self,
        *,
        line: int | None = None,
    ) -> None:
        """Takes one indentation step off a line."""
        args: dict[str, Any] = {}
        if line is not None:
            args["line"] = line
        return self.action("outdent_line", **args)

    def outdent_line_many(self, items: list[OutdentLineArgs]) -> list[dict[str, Any]]:
        """Runs `outdent_line` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("outdent_line", items)

    def find_text(self,
        *,
        text: str,
        case: bool | None = None,
        word: bool | None = None,
    ) -> list[Found]:
        """Finds the words `text` in a file, the case of their letters aside unless `case` says otherwise; `word` matches only where they stand alone as a word."""
        args: dict[str, Any] = {}
        args["text"] = text
        if case is not None:
            args["case"] = case
        if word is not None:
            args["word"] = word
        return self.action("find_text", **args)

    def replace_in_file(self,
        *,
        text: str,
        with_: str,
        case: bool | None = None,
        word: bool | None = None,
    ) -> int:
        """Replaces every place the words `text` stand in a file, answering how many changed."""
        args: dict[str, Any] = {}
        args["text"] = text
        args["with"] = with_
        if case is not None:
            args["case"] = case
        if word is not None:
            args["word"] = word
        return self.action("replace_in_file", **args)

    def symbols(self) -> list[Found]:
        """The declarations a file's text writes — `fn`, `struct`, `enum`, `trait`, `class`, `def`, `interface`, `type`, `const` — each with the line and column it stands at: what the project's symbols are and what go-to-symbol offers."""
        return self.action("symbols")

    def reveal_line(self,
        *,
        line: int | None = None,
        column: int | None = None,
    ) -> None:
        """Shows `line` of `entry`, counted from 0: the editor scrolls it into view with the caret on it."""
        args: dict[str, Any] = {}
        if line is not None:
            args["line"] = line
        if column is not None:
            args["column"] = column
        return self.action("reveal_line", **args)

    def reveal_line_many(self, items: list[RevealLineArgs]) -> list[dict[str, Any]]:
        """Runs `reveal_line` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("reveal_line", items)

    def add_child(self) -> None:
        """Starts a file inside the folder `e`: a row's '+', pressed."""
        return self.action("add_child")

    def add_child_many(self, items: list[AddChildArgs]) -> list[dict[str, Any]]:
        """Runs `add_child` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_child", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"set_language", "set_language_many", "replace_text", "replace_text_many", "replace_range", "replace_range_many", "duplicate_line", "duplicate_line_many", "move_line", "move_line_many", "delete_line", "delete_line_many", "toggle_comment", "toggle_comment_many", "indent_line", "indent_line_many", "outdent_line", "outdent_line_many", "find_text", "replace_in_file", "symbols", "reveal_line", "reveal_line_many", "add_child", "add_child_many"})


class Problem(Resource):
    """Problem — `tl.so.tilo.code-editor.Problem`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    nbyucnyk: Severity  # severity
    cg3ctzdq: int  # line
    f5kva2nw: int  # column
    e35xwjly: str  # message


class Found(Resource):
    """Found — `tl.so.tilo.code-editor.Found`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    hzzanee7: int  # line
    qt6b4p4a: int  # column
    @property
    def _55vdsdve(self) -> str:
        """words (`55vdsdve` on the wire)."""
        return self.fields["55vdsdve"]

    @_55vdsdve.setter
    def _55vdsdve(self, value: str) -> None:
        self.edit("55vdsdve", value=value)


class Item(Resource):
    """Item — `tl.so.tilo.code-editor.Item`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    gvqsbkfl: ItemKind  # kind
    @property
    def _6zcxi5vb(self) -> str:
        """text (`6zcxi5vb` on the wire)."""
        return self.fields["6zcxi5vb"]

    @_6zcxi5vb.setter
    def _6zcxi5vb(self, value: str) -> None:
        self.edit("6zcxi5vb", value=value)
    effdgz7q: str  # path
    @property
    def _62tkoolt(self) -> int:
        """line (`62tkoolt` on the wire)."""
        return self.fields["62tkoolt"]

    @_62tkoolt.setter
    def _62tkoolt(self, value: int) -> None:
        self.edit("62tkoolt", value=value)
    @property
    def _3ritafbo(self) -> int:
        """column (`3ritafbo` on the wire)."""
        return self.fields["3ritafbo"]

    @_3ritafbo.setter
    def _3ritafbo(self, value: int) -> None:
        self.edit("3ritafbo", value=value)


class Command(Resource):
    """Command — `tl.so.tilo.code-editor.Command`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    c4ytwucq: str  # command
    i3avwdem: str  # cwd
    @property
    def _2xcd2xlu(self) -> str:
        """started (`2xcd2xlu` on the wire)."""
        return self.fields["2xcd2xlu"]

    @_2xcd2xlu.setter
    def _2xcd2xlu(self, value: str) -> None:
        self.edit("2xcd2xlu", value=value)
    thtznwi2: str  # ended
    vjb7fevj: int  # exit
    bukegscc: str  # output
    xa7laxtf: bool  # truncated
    dgchvwcx: str  # job
    ufboumgh: str  # state
    cck6yows: list[Item]  # items


class Hit(Resource):
    """Hit — `tl.so.tilo.code-editor.Hit`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    bcptqatu: str  # path
    zwn4bgwt: str  # rel
    bgpnyksl: int  # line
    hbulfnxb: int  # column
    uk7foyau: int  # len
    hl3ypi66: str  # preview


class Completion(Resource):
    """Completion — `tl.so.tilo.code-editor.Completion`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    qg4m5uwr: str  # label
    xm4myjqj: str  # detail
    kwj5llbq: str  # insert


class Location(Resource):
    """Location — `tl.so.tilo.code-editor.Location`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    trhzxars: str  # path
    pu4qmppj: int  # line
    e2bphgrg: int  # column


class Loaded(Resource):
    """Loaded — `tl.so.tilo.code-editor.Loaded`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    pisboohb: str  # text
    wcz4xfj7: int  # mtime
    jqqoxnpy: int  # len


class Stamped(Resource):
    """Stamped — `tl.so.tilo.code-editor.Stamped`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    fj7hqru4: int  # mtime
    qw5jig3a: int  # len


class Exists(Resource):
    """Exists — `tl.so.tilo.code-editor.Exists`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    uvwwyzvs: bool  # exists
    @property
    def _7n6rmzco(self) -> int:
        """mtime (`7n6rmzco` on the wire)."""
        return self.fields["7n6rmzco"]

    @_7n6rmzco.setter
    def _7n6rmzco(self, value: int) -> None:
        self.edit("7n6rmzco", value=value)
    pwjqsxzz: int  # len


class DiskAsk(Resource):
    """DiskAsk — `tl.so.tilo.code-editor.DiskAsk`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    kn6q47ka: str  # path


class DiskSave(Resource):
    """DiskSave — `tl.so.tilo.code-editor.DiskSave`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    haepmky3: str  # path
    pwykv2y3: str  # text
    zetzlvco: int  # mtime
    evmprhon: int  # len
    om5axsdm: bool  # force


class DiskEntry(Resource):
    """DiskEntry — `tl.so.tilo.code-editor.DiskEntry`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    bfnbknrx: str  # name
    wv4a7ixx: str  # path
    @property
    def _7z4nle5w(self) -> bool:
        """folder (`7z4nle5w` on the wire)."""
        return self.fields["7z4nle5w"]

    @_7z4nle5w.setter
    def _7z4nle5w(self, value: bool) -> None:
        self.edit("7z4nle5w", value=value)
    pp4ozwjd: bool  # hidden


class Listing(Resource):
    """Listing — `tl.so.tilo.code-editor.Listing`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    pd7egi2y: list[DiskEntry]  # entries


class SearchAsk(Resource):
    """SearchAsk — `tl.so.tilo.code-editor.SearchAsk`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    xkrqnga5: str  # root
    @property
    def _5dkbrhtv(self) -> str:
        """needle (`5dkbrhtv` on the wire)."""
        return self.fields["5dkbrhtv"]

    @_5dkbrhtv.setter
    def _5dkbrhtv(self, value: str) -> None:
        self.edit("5dkbrhtv", value=value)
    cdk5sbcj: bool  # case_sensitive
    b7eojwjq: bool  # whole_word


class RunAsk(Resource):
    """RunAsk — `tl.so.tilo.code-editor.RunAsk`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    qhqdbt2f: str  # command
    vqlkdbpf: str  # cwd


class JobAnswer(Resource):
    """JobAnswer — `tl.so.tilo.code-editor.JobAnswer`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    o7kaxwxn: str  # job


class SearchState(Resource):
    """SearchState — `tl.so.tilo.code-editor.SearchState`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    zby3g4ps: str  # state
    aqlgsja7: list[Hit]  # hits
    vp2ht5bq: int  # files
    izoyjzkp: bool  # truncated


class RunState(Resource):
    """RunState — `tl.so.tilo.code-editor.RunState`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    coxzozlk: str  # state
    swqx5bgj: int  # exit
    wsbgo4u6: str  # output
    vlearfbg: bool  # truncated
    iqbxloe7: str  # failure


class ServerAsk(Resource):
    """ServerAsk — `tl.so.tilo.code-editor.ServerAsk`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    dwxem6ty: str  # language
    fegqdch5: str  # root


class DocumentAsk(Resource):
    """DocumentAsk — `tl.so.tilo.code-editor.DocumentAsk`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    jmzsddw6: str  # job
    cimxyukl: str  # path
    u2ozcojk: str  # language
    kxtscxdf: str  # text
    xbl32ey4: int  # version


class AskAsk(Resource):
    """AskAsk — `tl.so.tilo.code-editor.AskAsk`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    gxbdmbnd: str  # job
    ysxutqlg: str  # path
    p4m2am32: int  # line
    q3inki66: int  # column
    l45wtegn: str  # what


class Progress(Resource):
    """Progress — `tl.so.tilo.code-editor.Progress`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    agn2f2db: str  # title
    g7hzlu2h: str  # detail
    whcglspc: int  # percent


class Diag(Resource):
    """Diag — `tl.so.tilo.code-editor.Diag`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    ioyqdqpm: str  # severity
    beb3ijea: int  # line
    mev4fmk5: int  # column
    iwar3uyn: str  # message


class CompletionAnswer(Resource):
    """CompletionAnswer — `tl.so.tilo.code-editor.CompletionAnswer`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    z5gkzdtb: str  # what
    xm2tpahh: list[Completion]  # items


class HoverAnswer(Resource):
    """HoverAnswer — `tl.so.tilo.code-editor.HoverAnswer`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    jchhxxfm: str  # what
    m4blp7e7: str  # text


class DefinitionAnswer(Resource):
    """DefinitionAnswer — `tl.so.tilo.code-editor.DefinitionAnswer`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    qb2osbdn: str  # what
    u57x3dub: list[Location]  # locations


class FormatEdit(Resource):
    """FormatEdit — `tl.so.tilo.code-editor.FormatEdit`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    @property
    def _7flc4fke(self) -> int:
        """start (`7flc4fke` on the wire)."""
        return self.fields["7flc4fke"]

    @_7flc4fke.setter
    def _7flc4fke(self, value: int) -> None:
        self.edit("7flc4fke", value=value)
    g2cgs6ok: int  # start_column
    wfsos5vr: int  # end
    jhfxuyi3: int  # end_column
    yj73ybbe: str  # text


class FormatAnswer(Resource):
    """FormatAnswer — `tl.so.tilo.code-editor.FormatAnswer`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    um2x3bkg: str  # what
    wikls7kq: list[FormatEdit]  # edits
    zke66gso: str  # error


class ServerProgress(Resource):
    """ServerProgress — `tl.so.tilo.code-editor.ServerProgress`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    b22hm43k: str  # title
    @property
    def _33vetzuz(self) -> str:
        """detail (`33vetzuz` on the wire)."""
        return self.fields["33vetzuz"]

    @_33vetzuz.setter
    def _33vetzuz(self, value: str) -> None:
        self.edit("33vetzuz", value=value)
    xvz6ifep: int | None  # percent


class ServerState(Resource):
    """ServerState — `tl.so.tilo.code-editor.ServerState`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    mxvtycpj: str  # state
    kqcbbx2v: str  # server
    @property
    def _66ixtrna(self) -> str:
        """failure (`66ixtrna` on the wire)."""
        return self.fields["66ixtrna"]

    @_66ixtrna.setter
    def _66ixtrna(self, value: str) -> None:
        self.edit("66ixtrna", value=value)
    sqfl7jdi: ServerProgress | None  # progress
    s4bc4e6s: dict[str, list[Diag]] | None  # diagnostics
    h4pgltlo: str  # answer


class Project(Resource):
    """Project — `tl.so.tilo.code-editor.Project`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"
    evbsknez: list[Entry[Entry]]  # files
    inupl3md: list[Entry[Command]]  # commands
    ubr6i22h: str  # folder
    @property
    def _7sbddtl7(self) -> str:
        """server_job (`7sbddtl7` on the wire)."""
        return self.fields["7sbddtl7"]

    @_7sbddtl7.setter
    def _7sbddtl7(self, value: str) -> None:
        self.edit("7sbddtl7", value=value)
    w3s6k4wz: str  # server_language
    tz2cc24s: str  # server_name
    wajnagjm: str  # server_state
    mkyngogi: str  # server_trouble
    jwtdk5zn: str  # server_progress
    yukh3x5s: bool  # language_servers
    @property
    def _5qk4qm7f(self) -> bool:
        """format_on_save (`5qk4qm7f` on the wire)."""
        return self.fields["5qk4qm7f"]

    @_5qk4qm7f.setter
    def _5qk4qm7f(self, value: bool) -> None:
        self.edit("5qk4qm7f", value=value)
    eolbog7w: str  # answer_what
    luh6yto7: str  # answer_text
    @property
    def _5vjxpshd(self) -> int:
        """answer_line (`5vjxpshd` on the wire)."""
        return self.fields["5vjxpshd"]

    @_5vjxpshd.setter
    def _5vjxpshd(self, value: int) -> None:
        self.edit("5vjxpshd", value=value)
    w3z62her: int  # answer_column
    p4n4bpno: list[Completion]  # answer_items
    thlj3m5d: list[Location]  # answer_locations
    t4nipezs: str  # search_job
    nbqioo6i: str  # search_query
    @property
    def _5z5b5fqg(self) -> str:
        """search_text (`5z5b5fqg` on the wire)."""
        return self.fields["5z5b5fqg"]

    @_5z5b5fqg.setter
    def _5z5b5fqg(self, value: str) -> None:
        self.edit("5z5b5fqg", value=value)
    @property
    def _7tocyk5d(self) -> bool:
        """search_case (`7tocyk5d` on the wire)."""
        return self.fields["7tocyk5d"]

    @_7tocyk5d.setter
    def _7tocyk5d(self, value: bool) -> None:
        self.edit("7tocyk5d", value=value)
    t6mgiinw: bool  # search_word
    of2jzwnc: str  # search_state
    nmqjtfyh: list[Hit]  # search_hits
    @property
    def _3gaova7k(self) -> int:
        """search_files (`3gaova7k` on the wire)."""
        return self.fields["3gaova7k"]

    @_3gaova7k.setter
    def _3gaova7k(self, value: int) -> None:
        self.edit("3gaova7k", value=value)
    w3quhvl4: bool  # search_truncated
    b5q44glb: list[Found]  # found
    vwrqicgc: str  # trouble

    def new_file(self,
        *,
        name: str | None = None,
        folder: Entry | None = None,
        text: str | None = None,
    ) -> Entry:
        """Starts a file called `name` in `folder`, holding `text`: what a person types in the project, what a drop makes, and where an import lands."""
        args: dict[str, Any] = {}
        if name is not None:
            args["name"] = name
        if folder is not None:
            args["folder"] = folder
        if text is not None:
            args["text"] = text
        return self.action("new_file", **args)

    def new_folder(self,
        *,
        name: str | None = None,
        folder: Entry | None = None,
    ) -> Entry:
        """Starts a folder called `name` in `folder`."""
        args: dict[str, Any] = {}
        if name is not None:
            args["name"] = name
        if folder is not None:
            args["folder"] = folder
        return self.action("new_folder", **args)

    def rename(self,
        *,
        entry: Entry,
        name: str,
    ) -> None:
        """Renames `entry` to `name`, keeping the names in its folder apart."""
        return self.action("rename", entry=entry, name=name)

    def rename_many(self, items: list[RenameArgs]) -> list[dict[str, Any]]:
        """Runs `rename` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename", items)

    def move_entry(self,
        *,
        entry: Entry,
        folder: Entry | None = None,
        before: Entry | None = None,
    ) -> None:
        """Moves `entry` into `folder` (the top of the project when none), just before `before` — or after every entry there when none names one."""
        args: dict[str, Any] = {}
        args["entry"] = entry
        if folder is not None:
            args["folder"] = folder
        if before is not None:
            args["before"] = before
        return self.action("move_entry", **args)

    def move_entry_many(self, items: list[MoveEntryArgs]) -> list[dict[str, Any]]:
        """Runs `move_entry` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_entry", items)

    def move_after(self,
        *,
        entry: Entry,
        after: Entry,
    ) -> None:
        """Moves `entry` just after `after`, as a drop on the row below asks."""
        return self.action("move_after", entry=entry, after=after)

    def move_after_many(self, items: list[MoveAfterArgs]) -> list[dict[str, Any]]:
        """Runs `move_after` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_after", items)

    def delete(self, *, entry: Entry) -> None:
        """Deletes `entry`, and every entry under it: a folder empties with its entries, as one change a person takes back at once."""
        return self.action("delete", entry=entry)

    def delete_many(self, items: list[DeleteArgs]) -> list[dict[str, Any]]:
        """Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete", items)

    def find_in_file(self,
        *,
        entry: Entry,
        text: str,
        case: bool | None = None,
        word: bool | None = None,
    ) -> None:
        """Finds the words `text` in a file and keeps what it found, for the page's find row."""
        args: dict[str, Any] = {}
        args["entry"] = entry
        args["text"] = text
        if case is not None:
            args["case"] = case
        if word is not None:
            args["word"] = word
        return self.action("find_in_file", **args)

    def find_in_file_many(self, items: list[FindInFileArgs]) -> list[dict[str, Any]]:
        """Runs `find_in_file` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("find_in_file", items)

    def check(self, *, entry: Entry) -> None:
        """Checks a file and keeps what it found: the device's server's problems when one answers for the file, the editor's own otherwise."""
        return self.action("check", entry=entry)

    def check_many(self, items: list[CheckArgs]) -> list[dict[str, Any]]:
        """Runs `check` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("check", items)

    def open_folder(self, *, folder: str) -> None:
        """Opens the folder `folder` as the project: each file and folder it holds, read as entries, and each file's bytes as its text."""
        return self.action("open_folder", folder=folder)

    def open_folder_many(self, items: list[OpenFolderArgs]) -> list[dict[str, Any]]:
        """Runs `open_folder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("open_folder", items)

    def read_into(self,
        *,
        answer: str,
        parent: Entry | None = None,
    ) -> None:
        """Records the entries `answer` lists, standing in `folder`, and reads a file's own bytes: the tree is the project's, so every file the project shows is one a save can check. The listing crosses an action call as the text the device answered, which it already is."""
        args: dict[str, Any] = {}
        if parent is not None:
            args["parent"] = parent
        args["answer"] = answer
        return self.action("read_into", **args)

    def read_into_many(self, items: list[ReadIntoArgs]) -> list[dict[str, Any]]:
        """Runs `read_into` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("read_into", items)

    def read_folder_into(self,
        *,
        folder: Entry,
        path: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("read_folder_into", folder=folder, path=path)

    def read_folder_into_many(self, items: list[ReadFolderIntoArgs]) -> list[dict[str, Any]]:
        """Runs `read_folder_into` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("read_folder_into", items)

    def read_text(self, *, entry: Entry) -> None:
        """Runs the app's action."""
        return self.action("read_text", entry=entry)

    def read_text_many(self, items: list[ReadTextArgs]) -> list[dict[str, Any]]:
        """Runs `read_text` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("read_text", items)

    def open_file(self, *, file: Any) -> None:
        """Opens a file a person chose, or one Files or another app handed over: `file` is the file it is, and its words become an entry of the project."""
        return self.action("open_file", file=file)

    def open_file_many(self, items: list[OpenFileArgs]) -> list[dict[str, Any]]:
        """Runs `open_file` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("open_file", items)

    def save(self, *, entry: Entry) -> None:
        """Writes a file's text to the device, checking first that the bytes there are the ones this project read: a file something else changed is not written over without the person saying so."""
        return self.action("save", entry=entry)

    def save_many(self, items: list[SaveArgs]) -> list[dict[str, Any]]:
        """Runs `save` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("save", items)

    def write(self,
        *,
        entry: Entry,
        force: bool,
    ) -> bool:
        """Runs the app's action."""
        return self.action("write", entry=entry, force=force)

    def overwrite(self, *, entry: Entry) -> None:
        """Saves a file over the device's change, as a person chose after reading what changed."""
        return self.action("overwrite", entry=entry)

    def overwrite_many(self, items: list[OverwriteArgs]) -> list[dict[str, Any]]:
        """Runs `overwrite` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("overwrite", items)

    def reload(self, *, entry: Entry) -> None:
        """Reads the file again from the device, dropping what changed here."""
        return self.action("reload", entry=entry)

    def reload_many(self, items: list[ReloadArgs]) -> list[dict[str, Any]]:
        """Runs `reload` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("reload", items)

    def save_all(self) -> None:
        """Writes every file that changed here to the device."""
        return self.action("save_all")

    def save_all_many(self, items: list[SaveAllArgs]) -> list[dict[str, Any]]:
        """Runs `save_all` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("save_all", items)

    def save_into(self,
        *,
        entry: Entry,
        files: list[Any],
    ) -> None:
        """Writes `entry` into the folder `files` names, under its own name."""
        return self.action("save_into", entry=entry, files=files)

    def save_into_many(self, items: list[SaveIntoArgs]) -> list[dict[str, Any]]:
        """Runs `save_into` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("save_into", items)

    def move_on_disk(self,
        *,
        entry: Entry,
        to: str,
    ) -> None:
        """Moves a file into the folder `to` of the project, on the device as in the tree."""
        return self.action("move_on_disk", entry=entry, to=to)

    def move_on_disk_many(self, items: list[MoveOnDiskArgs]) -> list[dict[str, Any]]:
        """Runs `move_on_disk` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_on_disk", items)

    def import_dropped(self, *, files: list[Any]) -> None:
        """Imports the files a person dropped on the window: a folder becomes a folder of the project, a text file an entry with its words."""
        return self.action("import_dropped", files=files)

    def import_dropped_many(self, items: list[ImportDroppedArgs]) -> list[dict[str, Any]]:
        """Runs `import_dropped` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import_dropped", items)

    def run(self,
        *,
        command: str,
        cwd: str | None = None,
    ) -> None:
        """Runs `command` in the project's folder, as the terminal does: its output and its exit status are kept with the command, and a person reads them beside the code."""
        args: dict[str, Any] = {}
        args["command"] = command
        if cwd is not None:
            args["cwd"] = cwd
        return self.action("run", **args)

    def run_many(self, items: list[RunArgs]) -> list[dict[str, Any]]:
        """Runs `run` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("run", items)

    def stop(self) -> None:
        """Stops the command that runs in the project now."""
        return self.action("stop")

    def stop_many(self, items: list[StopArgs]) -> list[dict[str, Any]]:
        """Runs `stop` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("stop", items)

    def rerun(self, *, run: Command) -> None:
        """Runs a command of the project's terminal again, as a person does from its history."""
        return self.action("rerun", run=run)

    def rerun_many(self, items: list[RerunArgs]) -> list[dict[str, Any]]:
        """Runs `rerun` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rerun", items)

    def rerun_test(self,
        *,
        run: Command,
        test: str,
    ) -> None:
        """Runs one test of a run again: the command with the test's name after it, run as that run's tool ran it."""
        return self.action("rerun_test", run=run, test=test)

    def rerun_test_many(self, items: list[RerunTestArgs]) -> list[dict[str, Any]]:
        """Runs `rerun_test` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rerun_test", items)

    def run_(self,
        *,
        cwd: str,
        command: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("run_", cwd=cwd, command=command)

    def run__many(self, items: list[ProjectRunArgs]) -> list[dict[str, Any]]:
        """Runs `run_` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("run_", items)

    def clear_commands(self) -> None:
        """Erases the terminal's history."""
        return self.action("clear_commands")

    def clear_commands_many(self, items: list[ClearCommandsArgs]) -> list[dict[str, Any]]:
        """Runs `clear_commands` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_commands", items)

    def open_terminal(self) -> None:
        """Opens the project's folder in the system's terminal: the shell a person runs by hand."""
        return self.action("open_terminal")

    def open_terminal_many(self, items: list[OpenTerminalArgs]) -> list[dict[str, Any]]:
        """Runs `open_terminal` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("open_terminal", items)

    def poll(self) -> None:
        """Asks how far the running commands and the project search have got, once a second while anything runs: the one place the device's answers are folded into what the window shows."""
        return self.action("poll")

    def poll_many(self, items: list[PollArgs]) -> list[dict[str, Any]]:
        """Runs `poll` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("poll", items)

    def reveal_item(self,
        *,
        path: str,
        line: int | None = None,
    ) -> None:
        """Brings the caret to the place an item of a command's output names: the file is read if the project does not hold it yet, and the page shows it."""
        args: dict[str, Any] = {}
        args["path"] = path
        if line is not None:
            args["line"] = line
        return self.action("reveal_item", **args)

    def reveal_item_many(self, items: list[RevealItemArgs]) -> list[dict[str, Any]]:
        """Runs `reveal_item` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("reveal_item", items)

    def go_to_line(self,
        *,
        path: str,
        line: int | None = None,
    ) -> None:
        """Shows `line` of the file at `path`, counted from 0."""
        args: dict[str, Any] = {}
        args["path"] = path
        if line is not None:
            args["line"] = line
        return self.action("go_to_line", **args)

    def go_to_line_many(self, items: list[GoToLineArgs]) -> list[dict[str, Any]]:
        """Runs `go_to_line` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("go_to_line", items)

    def search_project(self,
        *,
        needle: str,
        case: bool | None = None,
        word: bool | None = None,
    ) -> None:
        """Searches every file of the project for `needle`, the case of its letters aside unless `case` says otherwise."""
        args: dict[str, Any] = {}
        args["needle"] = needle
        if case is not None:
            args["case"] = case
        if word is not None:
            args["word"] = word
        return self.action("search_project", **args)

    def search_project_many(self, items: list[SearchProjectArgs]) -> list[dict[str, Any]]:
        """Runs `search_project` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("search_project", items)

    def stop_search(self) -> None:
        """Stops the project search."""
        return self.action("stop_search")

    def stop_search_many(self, items: list[StopSearchArgs]) -> list[dict[str, Any]]:
        """Runs `stop_search` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("stop_search", items)

    def open_hit(self,
        *,
        path: str,
        line: int | None = None,
    ) -> None:
        """Shows the place a search found: the file is read from the device when the project does not hold it yet, and the page opens it."""
        args: dict[str, Any] = {}
        args["path"] = path
        if line is not None:
            args["line"] = line
        return self.action("open_hit", **args)

    def open_hit_many(self, items: list[OpenHitArgs]) -> list[dict[str, Any]]:
        """Runs `open_hit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("open_hit", items)

    def ensure_server(self, *, entry: Entry) -> None:
        """Starts the language server for a file's language, if none runs for it yet, and tells it about the file."""
        return self.action("ensure_server", entry=entry)

    def ensure_server_many(self, items: list[EnsureServerArgs]) -> list[dict[str, Any]]:
        """Runs `ensure_server` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("ensure_server", items)

    def announce(self, *, entry: Entry) -> None:
        """Tells the server that `entry` is open, with the text it holds now, so its answers are about what a person reads."""
        return self.action("announce", entry=entry)

    def poll_server(self) -> None:
        """Reads what the server answered: its state and progress, its problems for each file, and any answer to a code-help ask the page made."""
        return self.action("poll_server")

    def poll_server_many(self, items: list[PollServerArgs]) -> list[dict[str, Any]]:
        """Runs `poll_server` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("poll_server", items)

    def read_answer(self, *, answer: str) -> None:
        """Reads the answer the server gave to the page's last ask: a completion list, the words a hover shows, the places a declaration stands at, or the edits a format makes."""
        return self.action("read_answer", answer=answer)

    def read_answer_many(self, items: list[ReadAnswerArgs]) -> list[dict[str, Any]]:
        """Runs `read_answer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("read_answer", items)

    def complete(self,
        *,
        entry: Entry,
        line: int | None = None,
        column: int | None = None,
    ) -> None:
        """Asks the server for completions at a place in a file."""
        args: dict[str, Any] = {}
        args["entry"] = entry
        if line is not None:
            args["line"] = line
        if column is not None:
            args["column"] = column
        return self.action("complete", **args)

    def complete_many(self, items: list[CompleteArgs]) -> list[dict[str, Any]]:
        """Runs `complete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("complete", items)

    def hover(self,
        *,
        entry: Entry,
        line: int | None = None,
        column: int | None = None,
    ) -> None:
        """Asks the server what stands at a place in a file."""
        args: dict[str, Any] = {}
        args["entry"] = entry
        if line is not None:
            args["line"] = line
        if column is not None:
            args["column"] = column
        return self.action("hover", **args)

    def hover_many(self, items: list[HoverArgs]) -> list[dict[str, Any]]:
        """Runs `hover` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("hover", items)

    def definition(self,
        *,
        entry: Entry,
        line: int | None = None,
        column: int | None = None,
    ) -> None:
        """Asks the server where the declaration at a place is written."""
        args: dict[str, Any] = {}
        args["entry"] = entry
        if line is not None:
            args["line"] = line
        if column is not None:
            args["column"] = column
        return self.action("definition", **args)

    def definition_many(self, items: list[DefinitionArgs]) -> list[dict[str, Any]]:
        """Runs `definition` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("definition", items)

    def accept_completion(self,
        *,
        entry: Entry,
        label: str,
        insert: str,
    ) -> None:
        """Writes the completion a person chose where the caret stands."""
        return self.action("accept_completion", entry=entry, label=label, insert=insert)

    def accept_completion_many(self, items: list[AcceptCompletionArgs]) -> list[dict[str, Any]]:
        """Runs `accept_completion` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("accept_completion", items)

    def format_file(self, *, entry: Entry) -> None:
        """Reformats a file as its language's formatter does, as one change."""
        return self.action("format_file", entry=entry)

    def format_file_many(self, items: list[FormatFileArgs]) -> list[dict[str, Any]]:
        """Runs `format_file` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("format_file", items)

    def toggle_language_servers(self,
        *,
        servers: bool | None = None,
    ) -> None:
        """Turns language servers on or off, for the project."""
        args: dict[str, Any] = {}
        if servers is not None:
            args["servers"] = servers
        return self.action("toggle_language_servers", **args)

    def toggle_language_servers_many(self, items: list[ToggleLanguageServersArgs]) -> list[dict[str, Any]]:
        """Runs `toggle_language_servers` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("toggle_language_servers", items)

    def toggle_format_on_save(self,
        *,
        format: bool | None = None,
    ) -> None:
        """Turns formatting a file on save on or off."""
        args: dict[str, Any] = {}
        if format is not None:
            args["format"] = format
        return self.action("toggle_format_on_save", **args)

    def toggle_format_on_save_many(self, items: list[ToggleFormatOnSaveArgs]) -> list[dict[str, Any]]:
        """Runs `toggle_format_on_save` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("toggle_format_on_save", items)

    def restart_server(self) -> None:
        """Stops the language server, as a person pressing 'Try again' after a failure does."""
        return self.action("restart_server")

    def restart_server_many(self, items: list[RestartServerArgs]) -> list[dict[str, Any]]:
        """Runs `restart_server` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("restart_server", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_file", "new_folder", "rename", "rename_many", "move_entry", "move_entry_many", "move_after", "move_after_many", "delete", "delete_many", "find_in_file", "find_in_file_many", "check", "check_many", "open_folder", "open_folder_many", "read_into", "read_into_many", "read_folder_into", "read_folder_into_many", "read_text", "read_text_many", "open_file", "open_file_many", "save", "save_many", "write", "overwrite", "overwrite_many", "reload", "reload_many", "save_all", "save_all_many", "save_into", "save_into_many", "move_on_disk", "move_on_disk_many", "import_dropped", "import_dropped_many", "run", "run_many", "stop", "stop_many", "rerun", "rerun_many", "rerun_test", "rerun_test_many", "run_", "run__many", "clear_commands", "clear_commands_many", "open_terminal", "open_terminal_many", "poll", "poll_many", "reveal_item", "reveal_item_many", "go_to_line", "go_to_line_many", "search_project", "search_project_many", "stop_search", "stop_search_many", "open_hit", "open_hit_many", "ensure_server", "ensure_server_many", "announce", "poll_server", "poll_server_many", "read_answer", "read_answer_many", "complete", "complete_many", "hover", "hover_many", "definition", "definition_many", "accept_completion", "accept_completion_many", "format_file", "format_file_many", "toggle_language_servers", "toggle_language_servers_many", "toggle_format_on_save", "toggle_format_on_save_many", "restart_server", "restart_server_many"})


class CodeEditor(Resource):
    """Code Editor — `tl.so.tilo.code-editor`."""
    __surface__ = "so.tilo.code-editor@5acf667865adaa72"

