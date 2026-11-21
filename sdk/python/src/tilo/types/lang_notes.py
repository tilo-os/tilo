"""Types for Notes (`so.tilo.lang.notes`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Note` is the app's `tl.so.tilo.lang.notes.Note` at `notes/<resource>`, read with
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
    "Note",
    "Notebook",
    "Notes",
    "PinArgs",
    "RetitleArgs",
    "AddTextArgs",
    "TagArgs",
    "UntagArgs",
    "KeepImageArgs",
    "MoveToArgs",
    "MoveAfterArgs",
    "SortChildrenArgs",
    "DeleteArgs",
    "AppendToNoteArgs",
]

PinArgs = TypedDict("PinArgs", {
    "pinned": NotRequired["bool | None"],
})
"""The arguments of `pin`."""

RetitleArgs = TypedDict("RetitleArgs", {
    "title": "str",
})
"""The arguments of `retitle`."""

AddTextArgs = TypedDict("AddTextArgs", {
    "text": "str",
})
"""The arguments of `add_text`."""

TagArgs = TypedDict("TagArgs", {
    "t": "str",
})
"""The arguments of `tag`."""

UntagArgs = TypedDict("UntagArgs", {
    "t": "str",
})
"""The arguments of `untag`."""

KeepImageArgs = TypedDict("KeepImageArgs", {
    "picture": "list[str]",
})
"""The arguments of `keep_image`."""

MoveToArgs = TypedDict("MoveToArgs", {
    "moved": "Note",
    "parent": NotRequired["Note | None"],
    "before": NotRequired["Note | None"],
})
"""The arguments of `move_to`."""

MoveAfterArgs = TypedDict("MoveAfterArgs", {
    "moved": "Note",
    "n": "Note",
})
"""The arguments of `move_after`."""

SortChildrenArgs = TypedDict("SortChildrenArgs", {
    "n": "Note",
})
"""The arguments of `sort_children`."""

DeleteArgs = TypedDict("DeleteArgs", {
    "n": "Note",
})
"""The arguments of `delete`."""

AppendToNoteArgs = TypedDict("AppendToNoteArgs", {
    "title": "str",
    "text": "str",
})
"""The arguments of `append_to_note`."""

class Note(Resource):
    """Note — `tl.so.tilo.lang.notes.Note`."""
    __surface__ = "so.tilo.lang.notes@f28cdaaae36373f8"
    dpj7scfe: str  # title
    olsyp3aw: str  # body
    pmjerv63: dict[str, str]  # images
    ssu2tbnf: Note | None  # parent
    gfk6rqxp: bool  # pinned
    xcpasjg7: list[str]  # tags
    erijxfd3: str  # source
    uquoltho: str  # origin
    ngwafrjq: str  # by
    buuvovaf: str  # service
    jznzkj47: str  # page
    hchfoqqm: str  # created
    _5wdceapz: str  # updated

    def pin(self,
        *,
        pinned: bool | None = None,
    ) -> None:
        """Pins or unpins a note."""
        args: dict[str, Any] = {}
        if pinned is not None:
            args["pinned"] = pinned
        return self.action("pin", **args)

    def pin_many(self, items: list[PinArgs]) -> list[dict[str, Any]]:
        """Runs `pin` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("pin", items)

    def retitle(self, *, title: str) -> None:
        """Renames a note."""
        return self.action("retitle", title=title)

    def retitle_many(self, items: list[RetitleArgs]) -> list[dict[str, Any]]:
        """Runs `retitle` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("retitle", items)

    def add_text(self, *, text: str) -> None:
        """Writes `text` (Markdown) at the end of a note, under what it says already: a line, a list or a paragraph added to it."""
        return self.action("add_text", text=text)

    def add_text_many(self, items: list[AddTextArgs]) -> list[dict[str, Any]]:
        """Runs `add_text` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_text", items)

    def tag(self, *, t: str) -> None:
        """Gives a note a tag."""
        return self.action("tag", t=t)

    def tag_many(self, items: list[TagArgs]) -> list[dict[str, Any]]:
        """Runs `tag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("tag", items)

    def untag(self, *, t: str) -> None:
        """Untags a note."""
        return self.action("untag", t=t)

    def untag_many(self, items: list[UntagArgs]) -> list[dict[str, Any]]:
        """Runs `untag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("untag", items)

    def keep_image(self, *, picture: list[str]) -> None:
        """Keeps a picture the editor wrote into a note where the person pasted or dropped it: the note holds the address of its bytes, and the name it came in as, which an export writes beside it. `picture` is the editor's word of it: its name, then the address of its content."""
        return self.action("keep_image", picture=picture)

    def keep_image_many(self, items: list[KeepImageArgs]) -> list[dict[str, Any]]:
        """Runs `keep_image` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("keep_image", items)

    def export_markdown(self) -> str:
        """A note as Markdown: its title, then its body, each picture it shows written beside it as `<note>/images/<name>`, a path another reader of the file follows and this app reads again as the address of the picture's bytes."""
        return self.action("export_markdown")

    def export_note(self) -> Any:
        """A note as a file: its Markdown, and the pictures it shows beside it. A note that shows a picture is one zip — `Q3 plan.md` and `Q3 plan/images/fox.png` — because Markdown reaches a picture by a path and by nothing else; a note of words alone is that Markdown itself."""
        return self.action("export_note")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"pin", "pin_many", "retitle", "retitle_many", "add_text", "add_text_many", "tag", "tag_many", "untag", "untag_many", "keep_image", "keep_image_many", "export_markdown", "export_note"})


class Notebook(Resource):
    """Notebook — `tl.so.tilo.lang.notes.Notebook`."""
    __surface__ = "so.tilo.lang.notes@f28cdaaae36373f8"
    tmqqtkeb: list[Entry[Note]]  # notes

    def new_note(self,
        *,
        title: str | None = None,
        body: str | None = None,
    ) -> Note:
        """Starts a note at the top of the tree, titled `title` and saying `body` (Markdown) when the caller knows them already. Started from the notebook both are empty: what is typed first names it."""
        args: dict[str, Any] = {}
        if title is not None:
            args["title"] = title
        if body is not None:
            args["body"] = body
        return self.action("new_note", **args)

    def new_child(self, *, n: Note) -> Note:
        """Starts a note inside `n`'s own, after the notes it already holds."""
        return self.action("new_child", n=n)

    def move_to(self,
        *,
        moved: Note,
        parent: Note | None = None,
        before: Note | None = None,
    ) -> None:
        """Moves a note to a place in the tree: at the end of `parent`'s own, or where `before` stands."""
        args: dict[str, Any] = {}
        args["moved"] = moved
        if parent is not None:
            args["parent"] = parent
        if before is not None:
            args["before"] = before
        return self.action("move_to", **args)

    def move_to_many(self, items: list[MoveToArgs]) -> list[dict[str, Any]]:
        """Runs `move_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_to", items)

    def move_after(self,
        *,
        moved: Note,
        n: Note,
    ) -> None:
        """Moves a note to just after `n` and every note under it: the place a drop after a row asks for, which names the row it lands after rather than the row after that one."""
        return self.action("move_after", moved=moved, n=n)

    def move_after_many(self, items: list[MoveAfterArgs]) -> list[dict[str, Any]]:
        """Runs `move_after` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_after", items)

    def duplicate(self, *, n: Note) -> Note:
        """Copies a note, with every note under it, just after the one it copies."""
        return self.action("duplicate", n=n)

    def sort_children(self, *, n: Note) -> None:
        """Puts a note's own in the order of their titles, once, for a branch that grew out of order."""
        return self.action("sort_children", n=n)

    def sort_children_many(self, items: list[SortChildrenArgs]) -> list[dict[str, Any]]:
        """Runs `sort_children` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("sort_children", items)

    def delete(self, *, n: Note) -> None:
        """Deletes a note and every note under it, as one change a person takes back at once."""
        return self.action("delete", n=n)

    def delete_many(self, items: list[DeleteArgs]) -> list[dict[str, Any]]:
        """Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete", items)

    def append_to_note(self,
        *,
        title: str,
        text: str,
    ) -> None:
        """Writes `text` (Markdown) at the end of the note titled `title`, the case of its letters aside: how an agent or a script that knows a note by its name ('add this to Meeting notes') writes into it without first looking up which note that is."""
        return self.action("append_to_note", title=title, text=text)

    def append_to_note_many(self, items: list[AppendToNoteArgs]) -> list[dict[str, Any]]:
        """Runs `append_to_note` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("append_to_note", items)

    def file_note(self,
        *,
        title: str,
        tag: str,
    ) -> Note:
        """Files a note titled `title` and tagged `tag` in one step: what another app writes down of what it did (Today's morning) without waiting on the note it made to name it."""
        return self.action("file_note", title=title, tag=tag)

    def export_notebook(self) -> Any:
        """The notebook as a zip, one Markdown file for each note: a note's own under the folder of the note it stands in, and the pictures it shows in its own `images` folder beside it. That is the shape the import reads, so the notebook comes back as the tree it is, titles, bodies and pictures alike."""
        return self.action("export_notebook")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_note", "new_child", "move_to", "move_to_many", "move_after", "move_after_many", "duplicate", "sort_children", "sort_children_many", "delete", "delete_many", "append_to_note", "append_to_note_many", "file_note", "export_notebook"})


class Notes(Resource):
    """Notes — `tl.so.tilo.lang.notes`."""
    __surface__ = "so.tilo.lang.notes@f28cdaaae36373f8"

