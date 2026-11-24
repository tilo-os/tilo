"""Types for Gallery (`so.tilo.lang.gallery`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `gallery/<resource>`, read with
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
    "Kind",
    "Urgency",
    "Notice",
    "Item",
    "Gallery",
    "Row",
    "Node",
    "Message",
    "Card",
    "Change",
    "Photo",
    "Pinned",
    "LangGallery",
    "EditBlockArgs",
    "InsertBlockArgs",
    "DeleteBlockArgs",
    "FinishArgs",
    "ReactArgs",
    "GalleryEditBlockArgs",
    "GalleryInsertBlockArgs",
    "GalleryDeleteBlockArgs",
    "AddItemArgs",
    "RemoveItemArgs",
    "NoteChangeArgs",
    "SeekToArgs",
]

Kind = Literal["notes", "tasks", "mail"]
"""What a pinned app is, and the picture it shows on a dock."""

Urgency = Literal["none", "low", "medium", "high"]
"""How soon a report is due, as a case the report holds."""

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

FinishArgs = TypedDict("FinishArgs", {
})
"""The arguments of `finish`."""

ReactArgs = TypedDict("ReactArgs", {
    "emoji": "str",
})
"""The arguments of `react`."""

GalleryEditBlockArgs = TypedDict("GalleryEditBlockArgs", {
    "block": "str",
    "text": "str",
})
"""The arguments of `edit-block`."""

GalleryInsertBlockArgs = TypedDict("GalleryInsertBlockArgs", {
    "block": "str",
    "text": "str",
})
"""The arguments of `insert-block`."""

GalleryDeleteBlockArgs = TypedDict("GalleryDeleteBlockArgs", {
    "block": "str",
})
"""The arguments of `delete-block`."""

AddItemArgs = TypedDict("AddItemArgs", {
})
"""The arguments of `add_item`."""

RemoveItemArgs = TypedDict("RemoveItemArgs", {
    "x": "Item",
})
"""The arguments of `remove_item`."""

NoteChangeArgs = TypedDict("NoteChangeArgs", {
    "summary": "str",
})
"""The arguments of `note_change`."""

SeekToArgs = TypedDict("SeekToArgs", {
    "place": "float",
})
"""The arguments of `seek_to`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.lang.gallery@afae45cb5fc2be55"
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


class Item(Resource):
    """Item — `tl.so.tilo.lang.gallery.Item`."""
    __surface__ = "so.tilo.lang.gallery@afae45cb5fc2be55"
    n5clkmib: str  # title
    znpp3agb: str  # notes
    xttl6aec: str  # stage
    @property
    def _44yujnht(self) -> dict[str, list[str]]:
        """reactions (`44yujnht` on the wire)."""
        return self.fields["44yujnht"]

    @_44yujnht.setter
    def _44yujnht(self, value: dict[str, list[str]]) -> None:
        self.edit("44yujnht", value=value)

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

    def finish(self) -> None:
        """Marks an item done."""
        return self.action("finish")

    def finish_many(self, items: list[FinishArgs]) -> list[dict[str, Any]]:
        """Runs `finish` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("finish", items)

    def react(self, *, emoji: str) -> None:
        """Reacts to an item with an emoji, or takes the person's reaction back."""
        return self.action("react", emoji=emoji)

    def react_many(self, items: list[ReactArgs]) -> list[dict[str, Any]]:
        """Runs `react` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("react", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"edit_block", "edit_block_many", "insert_block", "insert_block_many", "delete_block", "delete_block_many", "finish", "finish_many", "react", "react_many"})


class Gallery(Resource):
    """Gallery — `tl.so.tilo.lang.gallery.Gallery`."""
    __surface__ = "so.tilo.lang.gallery@afae45cb5fc2be55"
    bmdkc22s: str  # name
    cbwtcaei: str  # basics_note
    kwuh65ek: str  # controls_title
    gfaekx4h: bool  # controls_by_email
    wqgo62si: bool  # controls_repeating
    @property
    def _4a4siddd(self) -> float:
        """controls_margin (`4a4siddd` on the wire)."""
        return self.fields["4a4siddd"]

    @_4a4siddd.setter
    def _4a4siddd(self, value: float) -> None:
        self.edit("4a4siddd", value=value)
    @property
    def _3aihctss(self) -> float:
        """controls_share (`3aihctss` on the wire)."""
        return self.fields["3aihctss"]

    @_3aihctss.setter
    def _3aihctss(self, value: float) -> None:
        self.edit("3aihctss", value=value)
    bfnlu2pr: str  # controls_room
    @property
    def _5rd763gr(self) -> Urgency:
        """controls_urgency (`5rd763gr` on the wire)."""
        return self.fields["5rd763gr"]

    @_5rd763gr.setter
    def _5rd763gr(self, value: Urgency) -> None:
        self.edit("5rd763gr", value=value)
    tov7hrtu: str | None  # controls_owner
    pwg7s3ul: str | None  # controls_reviewer
    jg35gcbg: str  # controls_starts
    ncm2bwon: str  # controls_clock
    hy2q7by2: str | None  # controls_ends
    izjturst: str  # controls_accent
    jggqxsf4: str  # controls_shortcut
    icvcwg6u: str  # controls_recipient
    socb2mg5: str  # controls_note
    jxnozq52: str  # controls_notes
    fqnths44: bool  # controls_background
    @property
    def _6fezt7u6(self) -> bool:
        """controls_lettering (`6fezt7u6` on the wire)."""
        return self.fields["6fezt7u6"]

    @_6fezt7u6.setter
    def _6fezt7u6(self, value: bool) -> None:
        self.edit("6fezt7u6", value=value)
    uuapjl23: bool  # states_off
    mes2qxjc: bool  # states_on
    @property
    def _6v7qgjgp(self) -> str:
        """states_name (`6v7qgjgp` on the wire)."""
        return self.fields["6v7qgjgp"]

    @_6v7qgjgp.setter
    def _6v7qgjgp(self, value: str) -> None:
        self.edit("6v7qgjgp", value=value)
    fja32xcb: str  # states_wrong
    dnr3rpdd: float  # states_share
    dgjzr3us: str  # states_room
    @property
    def _5fbblehc(self) -> list[Entry[Item]]:
        """backlog_items (`5fbblehc` on the wire)."""
        return self.fields["5fbblehc"]

    @_5fbblehc.setter
    def _5fbblehc(self, value: list[Entry[Item]]) -> None:
        self.edit("5fbblehc", value=value)
    rbbe5kqg: str  # backlog_draft
    frzefp4n: list[Entry[Item]]  # release_items
    upmpqtnv: list[Entry[Row]]  # data_rows
    oj2bybhm: list[Entry[Node]]  # data_files
    emr3i6eu: list[Entry[Message]]  # data_messages
    @property
    def _6jhjiv7f(self) -> list[Entry[Card]]:
        """data_cards (`6jhjiv7f` on the wire)."""
        return self.fields["6jhjiv7f"]

    @_6jhjiv7f.setter
    def _6jhjiv7f(self, value: list[Entry[Card]]) -> None:
        self.edit("6jhjiv7f", value=value)
    a3frzqb4: list[Entry[Change]]  # data_changes
    mo2q5faz: list[Entry[Photo]]  # media_photos
    qd46u3ke: float  # media_at
    @property
    def _3tdz6w5q(self) -> str:
        """media_source (`3tdz6w5q` on the wire)."""
        return self.fields["3tdz6w5q"]

    @_3tdz6w5q.setter
    def _3tdz6w5q(self, value: str) -> None:
        self.edit("3tdz6w5q", value=value)
    wivzmqm3: list[Entry[Pinned]]  # chrome_apps

    def edit_block(self,
        *,
        block: str,
        text: str,
    ) -> None:
        """Set one paragraph of a rich-text field to what a Markdown text says."""
        return self.action("edit-block", block=block, text=text)

    def edit_block_many(self, items: list[GalleryEditBlockArgs]) -> list[dict[str, Any]]:
        """Runs `edit-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("edit-block", items)

    def insert_block(self,
        *,
        block: str,
        text: str,
    ) -> None:
        """Put a paragraph before the one the address names."""
        return self.action("insert-block", block=block, text=text)

    def insert_block_many(self, items: list[GalleryInsertBlockArgs]) -> list[dict[str, Any]]:
        """Runs `insert-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("insert-block", items)

    def delete_block(self, *, block: str) -> None:
        """Remove the paragraph the address names, and the paragraphs it holds."""
        return self.action("delete-block", block=block)

    def delete_block_many(self, items: list[GalleryDeleteBlockArgs]) -> list[dict[str, Any]]:
        """Runs `delete-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-block", items)

    def add_item(self) -> None:
        """Adds what the backlog's line says as an item to do, and clears the line."""
        return self.action("add_item")

    def add_item_many(self, items: list[AddItemArgs]) -> list[dict[str, Any]]:
        """Runs `add_item` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_item", items)

    def remove_item(self, *, x: Item) -> None:
        """Takes an item off the backlog."""
        return self.action("remove_item", x=x)

    def remove_item_many(self, items: list[RemoveItemArgs]) -> list[dict[str, Any]]:
        """Runs `remove_item` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_item", items)

    def note_change(self, *, summary: str) -> None:
        """Note a change to the document, made by whoever asks for it."""
        return self.action("note_change", summary=summary)

    def note_change_many(self, items: list[NoteChangeArgs]) -> list[dict[str, Any]]:
        """Runs `note_change` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("note_change", items)

    def seek_to(self, *, place: float) -> None:
        """Move the song to a place in it, as a share of its length."""
        return self.action("seek_to", place=place)

    def seek_to_many(self, items: list[SeekToArgs]) -> list[dict[str, Any]]:
        """Runs `seek_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("seek_to", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"edit_block", "edit_block_many", "insert_block", "insert_block_many", "delete_block", "delete_block_many", "add_item", "add_item_many", "remove_item", "remove_item_many", "note_change", "note_change_many", "seek_to", "seek_to_many"})


class Row(Resource):
    """Row — `tl.so.tilo.lang.gallery.Row`."""
    __surface__ = "so.tilo.lang.gallery@afae45cb5fc2be55"
    od2pk6d3: str  # title
    jqhagvfx: str  # state
    pu34cefb: int  # size


class Node(Resource):
    """Node — `tl.so.tilo.lang.gallery.Node`."""
    __surface__ = "so.tilo.lang.gallery@afae45cb5fc2be55"
    sartpsxo: str  # title
    iqq34vjp: Node | None  # parent
    jd5rkocy: bool  # folder
    @property
    def _5dlddfkg(self) -> int:
        """size (`5dlddfkg` on the wire)."""
        return self.fields["5dlddfkg"]

    @_5dlddfkg.setter
    def _5dlddfkg(self, value: int) -> None:
        self.edit("5dlddfkg", value=value)


class Message(Resource):
    """Message — `tl.so.tilo.lang.gallery.Message`."""
    __surface__ = "so.tilo.lang.gallery@afae45cb5fc2be55"
    lcfap66g: str  # title
    ssylkjtu: str  # from
    vyexrz4i: str  # body
    thqsnyuw: str  # at
    wavjxiov: bool  # mine


class Card(Resource):
    """Card — `tl.so.tilo.lang.gallery.Card`."""
    __surface__ = "so.tilo.lang.gallery@afae45cb5fc2be55"
    y5gdkqew: str  # title
    ii4dp5pl: int  # count


class Change(Resource):
    """Change — `tl.so.tilo.lang.gallery.Change`."""
    __surface__ = "so.tilo.lang.gallery@afae45cb5fc2be55"
    dmejdwow: str  # title
    kbnmru3y: str  # summary


class Photo(Resource):
    """Photo — `tl.so.tilo.lang.gallery.Photo`."""
    __surface__ = "so.tilo.lang.gallery@afae45cb5fc2be55"
    @property
    def _6zq4etzb(self) -> str:
        """title (`6zq4etzb` on the wire)."""
        return self.fields["6zq4etzb"]

    @_6zq4etzb.setter
    def _6zq4etzb(self, value: str) -> None:
        self.edit("6zq4etzb", value=value)
    s74gxrkc: str  # place
    @property
    def _5d3oxbry(self) -> str:
        """picture (`5d3oxbry` on the wire)."""
        return self.fields["5d3oxbry"]

    @_5d3oxbry.setter
    def _5d3oxbry(self, value: str) -> None:
        self.edit("5d3oxbry", value=value)
    lb2wi6nq: bool  # tall


class Pinned(Resource):
    """Pinned — `tl.so.tilo.lang.gallery.Pinned`."""
    __surface__ = "so.tilo.lang.gallery@afae45cb5fc2be55"
    ciwf3kva: str  # title
    @property
    def _5tfi5mod(self) -> Kind:
        """kind (`5tfi5mod` on the wire)."""
        return self.fields["5tfi5mod"]

    @_5tfi5mod.setter
    def _5tfi5mod(self, value: Kind) -> None:
        self.edit("5tfi5mod", value=value)


class LangGallery(Resource):
    """Gallery — `tl.so.tilo.lang.gallery`."""
    __surface__ = "so.tilo.lang.gallery@afae45cb5fc2be55"
    def note_item(self,
        *,
        last: str,
        x: Item,
    ) -> None:
        """Note the item a row opened, in the page's answer."""
        return self.action("note_item", last=last, x=x)

    def picked(self,
        *,
        last: str,
        row: Row,
    ) -> None:
        """Note the row the table picked, in the page's answer."""
        return self.action("picked", last=last, row=row)

    def opened(self,
        *,
        last: str,
        row: Row,
    ) -> None:
        """Note the row the table opened, in the page's answer."""
        return self.action("opened", last=last, row=row)

    def picked_node(self,
        *,
        last: str,
        node: Node,
    ) -> None:
        """Note the file the tree picked, in the page's answer."""
        return self.action("picked_node", last=last, node=node)

    def opened_node(self,
        *,
        last: str,
        node: Node,
    ) -> None:
        """Note the file the tree opened, in the page's answer."""
        return self.action("opened_node", last=last, node=node)

    def opened_card(self,
        *,
        last: str,
        card: Card,
    ) -> None:
        """Note the card the grid opened, in the page's answer."""
        return self.action("opened_card", last=last, card=card)

    def mark_chosen(self,
        *,
        last: str,
        chosen: str,
    ) -> None:
        """Note the cells the sheet chose, in the page's answer."""
        return self.action("mark_chosen", last=last, chosen=chosen)

    def mark_edit(self,
        *,
        last: str,
        pairs: list[str],
    ) -> None:
        """Note the cells the sheet edited, in the page's answer."""
        return self.action("mark_edit", last=last, pairs=pairs)

    def mark_byte(self,
        *,
        last: str,
        at: list[str],
    ) -> None:
        """Note the byte the grid was pressed on, in the page's answer."""
        return self.action("mark_byte", last=last, at=at)

    def mark_key(self,
        *,
        last: str,
        pressed: str,
    ) -> None:
        """Note the key the grid was given, in the page's answer."""
        return self.action("mark_key", last=last, pressed=pressed)

    def mark_wheel(self,
        *,
        last: str,
        rows: int,
    ) -> None:
        """Note the rows the grid scrolled by, in the page's answer."""
        return self.action("mark_wheel", last=last, rows=rows)

    def mark_row(self,
        *,
        last: str,
        row: int,
    ) -> None:
        """Note the row the diff grid was pressed on, in the page's answer."""
        return self.action("mark_row", last=last, row=row)

    def reverted(self,
        *,
        last: str,
        change: Change,
    ) -> None:
        """Note the change the history took back, in the page's answer."""
        return self.action("reverted", last=last, change=change)

    def opened_change(self,
        *,
        last: str,
        change: Change,
    ) -> None:
        """Note the change the history opened, in the page's answer."""
        return self.action("opened_change", last=last, change=change)

    def picture_from(self, *, first: int) -> None:
        """A page shown in a picture, from the first to the last."""
        return self.action("picture_from", first=first)

    def mark_page(self,
        *,
        last: str,
        index: int,
    ) -> None:
        """Note the page the rail chose, in the window's last answer."""
        return self.action("mark_page", last=last, index=index)

    def mark_move(self,
        *,
        last: str,
        index: int,
        at: int,
    ) -> None:
        """Note where a page moved to, in the window's last answer."""
        return self.action("mark_move", last=last, index=index, at=at)

    def mark_paint(self,
        *,
        last: str,
        p: list[float],
    ) -> None:
        """Note a stroke on the paint surface, in the page's answer."""
        return self.action("mark_paint", last=last, p=p)

    def mark_files(self,
        *,
        last: str,
        files: list[Any],
    ) -> None:
        """Note the files dropped, in the window's last answer."""
        return self.action("mark_files", last=last, files=files)

    def note_photo(self,
        *,
        last: str,
        p: Photo,
    ) -> None:
        """Note the picture a card opened, in the page's answer."""
        return self.action("note_photo", last=last, p=p)

    def mark_moved(self,
        *,
        last: str,
        moved: Pinned,
        at: int,
    ) -> None:
        """Note the item moved, in the window's last answer."""
        return self.action("mark_moved", last=last, moved=moved, at=at)

    def dropped(self,
        *,
        last: str,
        target: Any,
    ) -> None:
        """Note what a drop target took, in the window's last answer."""
        return self.action("dropped", last=last, target=target)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"note_item", "picked", "opened", "picked_node", "opened_node", "opened_card", "mark_chosen", "mark_edit", "mark_byte", "mark_key", "mark_wheel", "mark_row", "reverted", "opened_change", "picture_from", "mark_page", "mark_move", "mark_paint", "mark_files", "note_photo", "mark_moved", "dropped"})


