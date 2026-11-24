"""Types for Notes (`so.tilo.notes`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Note` is the app's `tl.so.tilo.notes.Note` at `notes/<resource>`, read with
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
    "Tag",
    "Found",
    "Notes",
    "EditBlockArgs",
    "InsertBlockArgs",
    "DeleteBlockArgs",
    "PinArgs",
    "IndentArgs",
    "OutdentArgs",
    "MoveUpArgs",
    "MoveDownArgs",
    "RetitleArgs",
    "AddTextArgs",
    "TagArgs",
    "UntagArgs",
    "ReactArgs",
    "InsertImageArgs",
    "PlaceImageArgs",
    "KeepImageArgs",
    "MoveToArgs",
    "MoveAfterArgs",
    "SortChildrenArgs",
    "DeleteArgs",
    "AppendToNoteArgs",
    "RenameTagArgs",
    "DeleteTagArgs",
    "ImportArchiveArgs",
    "ImportDroppedArgs",
]

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

PinArgs = TypedDict("PinArgs", {
    "pinned": NotRequired["bool | None"],
})
"""The arguments of `pin`."""

IndentArgs = TypedDict("IndentArgs", {
    "above": NotRequired["Note | None"],
})
"""The arguments of `indent`."""

OutdentArgs = TypedDict("OutdentArgs", {
})
"""The arguments of `outdent`."""

MoveUpArgs = TypedDict("MoveUpArgs", {
    "above": NotRequired["Note | None"],
})
"""The arguments of `move_up`."""

MoveDownArgs = TypedDict("MoveDownArgs", {
})
"""The arguments of `move_down`."""

RetitleArgs = TypedDict("RetitleArgs", {
    "title": "str",
})
"""The arguments of `retitle`."""

AddTextArgs = TypedDict("AddTextArgs", {
    "text": "str",
})
"""The arguments of `add_text`."""

TagArgs = TypedDict("TagArgs", {
    "tag": "str",
})
"""The arguments of `tag`."""

UntagArgs = TypedDict("UntagArgs", {
    "tag": "str",
})
"""The arguments of `untag`."""

ReactArgs = TypedDict("ReactArgs", {
    "emoji": "str",
})
"""The arguments of `react`."""

InsertImageArgs = TypedDict("InsertImageArgs", {
    "image": "Any",
    "alt": NotRequired["str | None"],
})
"""The arguments of `insert_image`."""

PlaceImageArgs = TypedDict("PlaceImageArgs", {
    "image": "Any",
})
"""The arguments of `place_image`."""

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

RenameTagArgs = TypedDict("RenameTagArgs", {
    "tag": "Tag",
    "wanted": "str",
})
"""The arguments of `rename_tag`."""

DeleteTagArgs = TypedDict("DeleteTagArgs", {
    "tag": "Tag",
})
"""The arguments of `delete_tag`."""

ImportArchiveArgs = TypedDict("ImportArchiveArgs", {
    "file": "Any",
})
"""The arguments of `import_archive`."""

ImportDroppedArgs = TypedDict("ImportDroppedArgs", {
    "files": "list[Any]",
})
"""The arguments of `import_dropped`."""

class Note(Resource):
    """Note — `tl.so.tilo.notes.Note`."""
    __surface__ = "so.tilo.notes@f93603daff5cced6"
    dpj7scfe: str  # title
    olsyp3aw: str  # body
    pmjerv63: dict[str, str]  # images
    ssu2tbnf: Note | None  # parent
    gfk6rqxp: bool  # pinned
    xcpasjg7: list[str]  # tags
    ddxl7oum: dict[str, list[str]]  # reactions
    t6igvr56: str  # reacted
    erijxfd3: str  # source
    uquoltho: str  # origin
    eo4enwxh: str  # by
    yzessjhr: str  # service
    zex2vawx: str  # page
    rnhwkv6z: str  # created
    @property
    def _2od7mwir(self) -> str:
        """updated (`2od7mwir` on the wire)."""
        return self.fields["2od7mwir"]

    @_2od7mwir.setter
    def _2od7mwir(self, value: str) -> None:
        self.edit("2od7mwir", value=value)
    q4w7m2xk: str  # chosen
    v8n3t5jb: int  # chosen_count

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

    def new_child(self,
        *,
        words: str | None = None,
    ) -> Note:
        """Starts a note inside `n`'s own, after the notes it already holds, titled `words` when the caller knows them already (a `/page` in one of `n`'s blocks names the page it makes with the words that stood there): the note the window has open too, so a person writing one starts the note it stands under without finding its row. It is `n`'s action, so the window offers it for the note it shows and its row offers it."""
        args: dict[str, Any] = {}
        if words is not None:
            args["words"] = words
        return self.action("new_child", **args)

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

    def indent(self,
        *,
        above: Note | None = None,
    ) -> None:
        """Puts a note under the note it stands over, with the notes under it, as `Tab` does in the tree; with none named, it goes under the note above it."""
        args: dict[str, Any] = {}
        if above is not None:
            args["above"] = above
        return self.action("indent", **args)

    def indent_many(self, items: list[IndentArgs]) -> list[dict[str, Any]]:
        """Runs `indent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("indent", items)

    def outdent(self) -> None:
        """Puts a note beside the note it stands in, just after it and the notes under it, as `Shift-Tab` does in the tree."""
        return self.action("outdent")

    def outdent_many(self, items: list[OutdentArgs]) -> list[dict[str, Any]]:
        """Runs `outdent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("outdent", items)

    def move_up(self,
        *,
        above: Note | None = None,
    ) -> None:
        """Puts a note just over the note above it, with the notes under it, as `Alt-Up` does in the tree; with none named, over the note just above it."""
        args: dict[str, Any] = {}
        if above is not None:
            args["above"] = above
        return self.action("move_up", **args)

    def move_up_many(self, items: list[MoveUpArgs]) -> list[dict[str, Any]]:
        """Runs `move_up` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_up", items)

    def move_down(self) -> None:
        """Puts a note just under the note below it and the notes under that one, with its own notes after it, as `Alt-Down` does in the tree."""
        return self.action("move_down")

    def move_down_many(self, items: list[MoveDownArgs]) -> list[dict[str, Any]]:
        """Runs `move_down` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_down", items)

    def retitle(self, *, title: str) -> None:
        """Renames a note."""
        return self.action("retitle", title=title)

    def retitle_many(self, items: list[RetitleArgs]) -> list[dict[str, Any]]:
        """Runs `retitle` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("retitle", items)

    def add_text(self, *, text: str) -> None:
        """Writes `text` (Markdown) at the end of a note, as paragraphs of its own under what it says already: a line, a list or a paragraph added to it, never running on into its last paragraph."""
        return self.action("add_text", text=text)

    def add_text_many(self, items: list[AddTextArgs]) -> list[dict[str, Any]]:
        """Runs `add_text` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_text", items)

    def tag(self, *, tag: str) -> None:
        """Gives a note the tag `tag`, and lists the tag in the notebook when it lists none of that name yet: a name is how a person, an agent and a script know a tag."""
        return self.action("tag", tag=tag)

    def tag_many(self, items: list[TagArgs]) -> list[dict[str, Any]]:
        """Runs `tag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("tag", items)

    def untag(self, *, tag: str) -> None:
        """Takes the tag `tag` off a note. The notebook keeps listing it, so a person finds it again for another note; deleting it from the notebook takes it off every note."""
        return self.action("untag", tag=tag)

    def untag_many(self, items: list[UntagArgs]) -> list[dict[str, Any]]:
        """Runs `untag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("untag", items)

    def find_text(self, *, text: str) -> list[Found]:
        """Finds the words `text` in one note, the case of their letters aside, and answers each paragraph holding them with its place in the note and its words: what the window's find bar shows, reachable by an agent or the palette that knows the note but not where in it to look."""
        return self.action("find_text", text=text)

    def react(self, *, emoji: str) -> None:
        """Adds your reaction with an emoji to a note, or takes it back when you had reacted with it: a press on one of its pills, or the pick of the one its add button opened."""
        return self.action("react", emoji=emoji)

    def react_many(self, items: list[ReactArgs]) -> list[dict[str, Any]]:
        """Runs `react` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("react", items)

    def insert_image(self,
        *,
        image: Any,
        alt: str | None = None,
    ) -> None:
        """Puts a picture in a note, where its words end, described by `alt` for a reader who cannot see it: the person picks the file, and the note keeps the address of its content rather than the picture, and the name it came in as. Nothing in `alt` can end the Markdown a picture is written with, so a description holding brackets or parentheses still reads as one picture."""
        args: dict[str, Any] = {}
        args["image"] = image
        if alt is not None:
            args["alt"] = alt
        return self.action("insert_image", **args)

    def insert_image_many(self, items: list[InsertImageArgs]) -> list[dict[str, Any]]:
        """Runs `insert_image` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("insert_image", items)

    def place_image(self, *, image: Any) -> None:
        """Writes a picture in where the person is writing: the picture the person chose is handed to the editor, which puts it at the caret, as its own `/image` command does. The note keeps the address of its bytes and the name it came in as, which an export writes beside the note."""
        return self.action("place_image", image=image)

    def place_image_many(self, items: list[PlaceImageArgs]) -> list[dict[str, Any]]:
        """Runs `place_image` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("place_image", items)

    def keep_image(self, *, picture: list[str]) -> None:
        """Keeps a picture the editor wrote into a note where the person pasted or dropped it: the note holds the address of its bytes, and the name it came in as, which an export writes beside it. `picture` is the editor's word of it: its name, then the address of its content. A picture already named by the note — one the person chose with `place_image` — keeps that name."""
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
    __actions__ = frozenset({"edit_block", "edit_block_many", "insert_block", "insert_block_many", "delete_block", "delete_block_many", "new_child", "pin", "pin_many", "indent", "indent_many", "outdent", "outdent_many", "move_up", "move_up_many", "move_down", "move_down_many", "retitle", "retitle_many", "add_text", "add_text_many", "tag", "tag_many", "untag", "untag_many", "find_text", "react", "react_many", "insert_image", "insert_image_many", "place_image", "place_image_many", "keep_image", "keep_image_many", "export_markdown", "export_note"})


class Notebook(Resource):
    """Notebook — `tl.so.tilo.notes.Notebook`."""
    __surface__ = "so.tilo.notes@f93603daff5cced6"
    tmqqtkeb: list[Entry[Note]]  # notes
    qgq6mcy7: list[Entry[Note]]  # set_aside
    dq2mnb22: list[Entry[Tag]]  # tags

    def new_note(self,
        *,
        title: str | None = None,
        body: str | None = None,
        parent: Note | None = None,
        after: Note | None = None,
    ) -> Note:
        """Starts a note titled `title` and saying `body` (Markdown) when the caller knows them already. Started from the notebook both are empty: what is typed first names it. It stands under `parent` (at the top of the tree when none) and — where a caller names the note it belongs after (`after`) — at that place among the notes there; without one it comes after the notes already under `parent`, never at the foot of a tree it does not stand in."""
        args: dict[str, Any] = {}
        if title is not None:
            args["title"] = title
        if body is not None:
            args["body"] = body
        if parent is not None:
            args["parent"] = parent
        if after is not None:
            args["after"] = after
        return self.action("new_note", **args)

    def search_notebook(self, *, text: str) -> None:
        """The window's search narrows the notebook's tree to the notes it finds, and opens the first of them: the note a person searched for is the one they read."""
        return self.action("search_notebook", text=text)

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

    def new_tag(self,
        *,
        name: str | None = None,
    ) -> Tag:
        """Makes a tag of the notebook named `name`, or gives the one already named that: the name is what people, agents and scripts know a tag by."""
        args: dict[str, Any] = {}
        if name is not None:
            args["name"] = name
        return self.action("new_tag", **args)

    def rename_tag(self,
        *,
        tag: Tag,
        wanted: str,
    ) -> None:
        """Names a tag of the notebook: every note that carries the old name carries the new one."""
        return self.action("rename_tag", tag=tag, wanted=wanted)

    def rename_tag_many(self, items: list[RenameTagArgs]) -> list[dict[str, Any]]:
        """Runs `rename_tag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename_tag", items)

    def delete_tag(self, *, tag: Tag) -> None:
        """Deletes a tag of the notebook: no note carries the name any more, and the notebook stops listing it."""
        return self.action("delete_tag", tag=tag)

    def delete_tag_many(self, items: list[DeleteTagArgs]) -> list[dict[str, Any]]:
        """Runs `delete_tag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_tag", items)

    def export_notebook(self) -> Any:
        """The notebook as a zip, one Markdown file for each note: a note's own under the folder of the note it stands in, and the pictures it shows in its own `images` folder beside it. That is the shape the import reads, so the notebook comes back as the tree it is, titles, bodies and pictures alike."""
        return self.action("export_notebook")

    def export_notebook_markdown(self) -> Any:
        """The notebook as one Markdown document, where each note is a heading one level deeper than the note it stands under (six at the deepest), its body beneath it and the notes under it after it, read as one text in the order the tree keeps it."""
        return self.action("export_notebook_markdown")

    def import_markdown(self, *, file: Any) -> Note:
        """Imports a Markdown file as a note, or opens the note that file already made: a note keeps the address of the file it came from, so importing the same file twice makes one note. The note stands under the note of the folder its file's path names — `Q3 plan/Ideas.md` under `Q3 plan.md` — and at the top of the tree when its path names no folder. Its title is the document's first heading, or the file's name; the heading it says the title with is not said again in its body."""
        return self.action("import_markdown", file=file)

    def import_page(self,
        *,
        file: Any,
        by: str,
        service: str,
        page: str,
        created: str | None = None,
        updated: str | None = None,
    ) -> Note:
        """Imports a page another service held as a note, keeping whose words it is and where it stood there: what `tilo migrate` calls for each page it brings over, and what a person's own import of a file does without a service's words. A page imported by its address there again opens the note it made, whatever the page says now; the note stands under the note of the folder its file's path names."""
        args: dict[str, Any] = {}
        args["file"] = file
        args["by"] = by
        args["service"] = service
        args["page"] = page
        if created is not None:
            args["created"] = created
        if updated is not None:
            args["updated"] = updated
        return self.action("import_page", **args)

    def import_text(self,
        *,
        title: str,
        markdown: str,
        pictures: list[Any] | None = None,
        links: dict[str, str] | None = None,
    ) -> Note:
        """Imports a note given as words rather than a file: its title, its body as Markdown, the pictures it shows and the notes its links lead to. A caller holding a page as text — a script, an agent, a migration — makes the note without writing a file first: `pictures` are the files the body names as `![alt](name)`, each by the name it came in as, which the note keeps so an export writes it beside the note again; `links` names, by the path a link leads to, the note it opens. The note stands at the top of the tree."""
        args: dict[str, Any] = {}
        args["title"] = title
        args["markdown"] = markdown
        if pictures is not None:
            args["pictures"] = pictures
        if links is not None:
            args["links"] = links
        return self.action("import_text", **args)

    def import_archive(self, *, file: Any) -> None:
        """Imports a notebook another tool wrote: a zip of pages, or the JSON a Notion API page listing answers. A zip comes in under the notebook, a page under the page that held it, in one change: undo takes the whole import back. That is this app's own notebook written out, a Notion export, and what `tilo migrate` brings from a wiki: a page per `.md` file, its pictures beside it, and a database beside it as a `.csv` that becomes a table at its page. Each page's title is its first heading, or its file's name; the pictures a page shows are the files it stands beside, a reference read from the page's own folder, and a link to a page of the archive is the reference to the note that page made. A migration says where each page came from in its own file beside the pages, and a page that is imported already keeps the note it made. A listing carries no files, so each of its pages is a note at the top of the tree."""
        return self.action("import_archive", file=file)

    def import_archive_many(self, items: list[ImportArchiveArgs]) -> list[dict[str, Any]]:
        """Runs `import_archive` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import_archive", items)

    def import_dropped(self, *, files: list[Any]) -> None:
        """Files dropped on the notebook come in one at a time. A Markdown file becomes a note. An archive or a Notion page listing becomes the pages it holds, whether the notebook's own written out or another app's export. One action, so undo takes the whole drop back at once."""
        return self.action("import_dropped", files=files)

    def import_dropped_many(self, items: list[ImportDroppedArgs]) -> list[dict[str, Any]]:
        """Runs `import_dropped` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import_dropped", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_note", "search_notebook", "move_to", "move_to_many", "move_after", "move_after_many", "duplicate", "sort_children", "sort_children_many", "delete", "delete_many", "append_to_note", "append_to_note_many", "file_note", "new_tag", "rename_tag", "rename_tag_many", "delete_tag", "delete_tag_many", "export_notebook", "export_notebook_markdown", "import_markdown", "import_page", "import_text", "import_archive", "import_archive_many", "import_dropped", "import_dropped_many"})


class Tag(Resource):
    """Tag — `tl.so.tilo.notes.Tag`."""
    __surface__ = "so.tilo.notes@f93603daff5cced6"
    itadoh74: str  # name


class Found(Resource):
    """Found — `tl.so.tilo.notes.Found`."""
    __surface__ = "so.tilo.notes@f93603daff5cced6"
    dxxdlopq: int  # at
    @property
    def _2vlaqp45(self) -> str:
        """words (`2vlaqp45` on the wire)."""
        return self.fields["2vlaqp45"]

    @_2vlaqp45.setter
    def _2vlaqp45(self, value: str) -> None:
        self.edit("2vlaqp45", value=value)


class Notes(Resource):
    """Notes — `tl.so.tilo.notes`."""
    __surface__ = "so.tilo.notes@f93603daff5cced6"

