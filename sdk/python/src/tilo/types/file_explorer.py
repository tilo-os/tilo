"""Types for Files (`so.tilo.file-explorer`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `file-explorer/<resource>`, read with
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
    "SortBy",
    "Notice",
    "Entry",
    "Listing",
    "Place",
    "Favorite",
    "Recent",
    "Preview",
    "Destination",
    "Transfer",
    "FileFacts",
    "TransferAsk",
    "TransferState",
    "Session",
    "Files",
    "Crumb",
    "FileExplorer",
    "TransferMovedArgs",
    "ReadListingArgs",
    "ChangedArgs",
    "PreviewStepArgs",
    "GoArgs",
    "BackArgs",
    "ForwardArgs",
    "UpArgs",
    "ShowArgs",
    "ExtractIntoArgs",
    "MoveIntoArgs",
    "DropIntoArgs",
    "CopyIntoArgs",
    "PutIntoArgs",
    "TransferEndedArgs",
    "DismissTransferArgs",
    "DismissNoticeArgs",
    "SortByArgs",
    "AscendingArgs",
    "ShowGridArgs",
    "ChooseViewArgs",
    "ToggleHiddenArgs",
    "SetZoomArgs",
    "ZoomInArgs",
    "ZoomOutArgs",
    "OpenEntryArgs",
    "FindOpenersArgs",
    "OpenWithArgs",
    "CloseOpenersArgs",
    "OpenTerminalArgs",
    "CompressEntryArgs",
    "ExtractEntryArgs",
    "TouchEntryArgs",
    "ChooseArgs",
    "SelectAllArgs",
    "ClearSelectionArgs",
    "NoteRecentArgs",
    "NewFolderArgs",
    "StartNewFileArgs",
    "NewFromTemplateArgs",
    "NewFileArgs",
    "ImportDroppedArgs",
    "RenameEntryArgs",
    "TrashEntriesArgs",
    "RestoreEntryArgs",
    "DeletePermanentlyArgs",
    "EmptyTrashArgs",
    "CarryArgs",
    "CarrySelectionArgs",
    "CompressSelectionArgs",
    "PasteArgs",
    "KeyOnArgs",
    "CarryRowArgs",
    "TrashRowArgs",
    "AddFavoriteArgs",
    "RemoveFavoriteArgs",
    "InspectArgs",
    "NameItArgs",
]

SortBy = Literal["name", "size", "modified", "format"]
"""How a folder's entries read."""

TransferMovedArgs = TypedDict("TransferMovedArgs", {
    "j": "Any",
})
"""The arguments of `transfer_moved`."""

ReadListingArgs = TypedDict("ReadListingArgs", {
    "place": NotRequired["str | None"],
})
"""The arguments of `read_listing`."""

ChangedArgs = TypedDict("ChangedArgs", {
})
"""The arguments of `changed`."""

PreviewStepArgs = TypedDict("PreviewStepArgs", {
})
"""The arguments of `preview_step`."""

GoArgs = TypedDict("GoArgs", {
    "place": "str",
})
"""The arguments of `go`."""

BackArgs = TypedDict("BackArgs", {
})
"""The arguments of `back`."""

ForwardArgs = TypedDict("ForwardArgs", {
})
"""The arguments of `forward`."""

UpArgs = TypedDict("UpArgs", {
})
"""The arguments of `up`."""

ShowArgs = TypedDict("ShowArgs", {
    "path": "str",
})
"""The arguments of `show`."""

ExtractIntoArgs = TypedDict("ExtractIntoArgs", {
    "path": "str",
    "folder": "str",
})
"""The arguments of `extract_into`."""

MoveIntoArgs = TypedDict("MoveIntoArgs", {
    "folder": "str",
})
"""The arguments of `move_into`."""

DropIntoArgs = TypedDict("DropIntoArgs", {
    "paths": "list[str]",
    "folder": "str",
})
"""The arguments of `drop_into`."""

CopyIntoArgs = TypedDict("CopyIntoArgs", {
    "folder": "str",
})
"""The arguments of `copy_into`."""

PutIntoArgs = TypedDict("PutIntoArgs", {
    "paths": "list[str]",
    "folder": "str",
    "moving": "bool",
})
"""The arguments of `put_into`."""

TransferEndedArgs = TypedDict("TransferEndedArgs", {
    "t": "Transfer",
    "j": "Any",
})
"""The arguments of `transfer_ended`."""

DismissTransferArgs = TypedDict("DismissTransferArgs", {
    "transfer": "Transfer",
})
"""The arguments of `dismiss_transfer`."""

DismissNoticeArgs = TypedDict("DismissNoticeArgs", {
})
"""The arguments of `dismiss_notice`."""

SortByArgs = TypedDict("SortByArgs", {
    "by": "SortBy",
})
"""The arguments of `sort_by`."""

AscendingArgs = TypedDict("AscendingArgs", {
    "first": "bool",
})
"""The arguments of `ascending`."""

ShowGridArgs = TypedDict("ShowGridArgs", {
    "grid": "bool",
})
"""The arguments of `show_grid`."""

ChooseViewArgs = TypedDict("ChooseViewArgs", {
    "i": "int",
})
"""The arguments of `choose_view`."""

ToggleHiddenArgs = TypedDict("ToggleHiddenArgs", {
    "hidden": "bool",
})
"""The arguments of `toggle_hidden`."""

SetZoomArgs = TypedDict("SetZoomArgs", {
    "step": "int",
})
"""The arguments of `set_zoom`."""

ZoomInArgs = TypedDict("ZoomInArgs", {
})
"""The arguments of `zoom_in`."""

ZoomOutArgs = TypedDict("ZoomOutArgs", {
})
"""The arguments of `zoom_out`."""

OpenEntryArgs = TypedDict("OpenEntryArgs", {
    "path": "str",
})
"""The arguments of `open_entry`."""

FindOpenersArgs = TypedDict("FindOpenersArgs", {
    "path": "str",
})
"""The arguments of `find_openers`."""

OpenWithArgs = TypedDict("OpenWithArgs", {
    "chosen": "str",
})
"""The arguments of `open_with`."""

CloseOpenersArgs = TypedDict("CloseOpenersArgs", {
})
"""The arguments of `close_openers`."""

OpenTerminalArgs = TypedDict("OpenTerminalArgs", {
    "folder": "str",
})
"""The arguments of `open_terminal`."""

CompressEntryArgs = TypedDict("CompressEntryArgs", {
    "path": "str",
    "zip": "bool",
})
"""The arguments of `compress_entry`."""

ExtractEntryArgs = TypedDict("ExtractEntryArgs", {
    "path": "str",
})
"""The arguments of `extract_entry`."""

TouchEntryArgs = TypedDict("TouchEntryArgs", {
    "path": "str",
})
"""The arguments of `touch_entry`."""

ChooseArgs = TypedDict("ChooseArgs", {
    "paths": "list[str]",
})
"""The arguments of `choose`."""

SelectAllArgs = TypedDict("SelectAllArgs", {
})
"""The arguments of `select_all`."""

ClearSelectionArgs = TypedDict("ClearSelectionArgs", {
})
"""The arguments of `clear_selection`."""

NoteRecentArgs = TypedDict("NoteRecentArgs", {
    "name": "str",
    "path": "str",
})
"""The arguments of `note_recent`."""

NewFolderArgs = TypedDict("NewFolderArgs", {
    "name": NotRequired["str | None"],
})
"""The arguments of `new_folder`."""

StartNewFileArgs = TypedDict("StartNewFileArgs", {
})
"""The arguments of `start_new_file`."""

NewFromTemplateArgs = TypedDict("NewFromTemplateArgs", {
    "template": "str",
    "typed": "str",
})
"""The arguments of `new_from_template`."""

NewFileArgs = TypedDict("NewFileArgs", {
    "name": NotRequired["str | None"],
})
"""The arguments of `new_file`."""

ImportDroppedArgs = TypedDict("ImportDroppedArgs", {
    "files": "list[Any]",
})
"""The arguments of `import_dropped`."""

RenameEntryArgs = TypedDict("RenameEntryArgs", {
    "path": "str",
    "name": "str",
})
"""The arguments of `rename_entry`."""

TrashEntriesArgs = TypedDict("TrashEntriesArgs", {
    "paths": "list[str]",
})
"""The arguments of `trash_entries`."""

RestoreEntryArgs = TypedDict("RestoreEntryArgs", {
    "name": "str",
})
"""The arguments of `restore_entry`."""

DeletePermanentlyArgs = TypedDict("DeletePermanentlyArgs", {
    "path": "str",
})
"""The arguments of `delete_permanently`."""

EmptyTrashArgs = TypedDict("EmptyTrashArgs", {
})
"""The arguments of `empty_trash`."""

CarryArgs = TypedDict("CarryArgs", {
    "paths": "list[str]",
    "cut": "bool",
})
"""The arguments of `carry`."""

CarrySelectionArgs = TypedDict("CarrySelectionArgs", {
    "cut": "bool",
})
"""The arguments of `carry_selection`."""

CompressSelectionArgs = TypedDict("CompressSelectionArgs", {
    "zip": "bool",
})
"""The arguments of `compress_selection`."""

PasteArgs = TypedDict("PasteArgs", {
})
"""The arguments of `paste`."""

KeyOnArgs = TypedDict("KeyOnArgs", {
    "focused": "str",
    "key": "str",
})
"""The arguments of `key_on`."""

CarryRowArgs = TypedDict("CarryRowArgs", {
    "path": "str",
    "cut": "bool",
})
"""The arguments of `carry_row`."""

TrashRowArgs = TypedDict("TrashRowArgs", {
    "path": "str",
})
"""The arguments of `trash_row`."""

AddFavoriteArgs = TypedDict("AddFavoriteArgs", {
    "place": "str",
})
"""The arguments of `add_favorite`."""

RemoveFavoriteArgs = TypedDict("RemoveFavoriteArgs", {
    "place": "str",
})
"""The arguments of `remove_favorite`."""

InspectArgs = TypedDict("InspectArgs", {
    "path": "str",
})
"""The arguments of `inspect`."""

NameItArgs = TypedDict("NameItArgs", {
    "naming": "str",
    "typed": "str",
})
"""The arguments of `name_it`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.file-explorer@32a9b53c26850c65"
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


class Entry(Resource):
    """Entry — `tl.so.tilo.file-explorer.Entry`."""
    __surface__ = "so.tilo.file-explorer@32a9b53c26850c65"
    lm429k0c: str  # name
    sq7vzl81: str  # path
    @property
    def _7x0zd867(self) -> bool:
        """folder (`7x0zd867` on the wire)."""
        return self.fields["7x0zd867"]

    @_7x0zd867.setter
    def _7x0zd867(self, value: bool) -> None:
        self.edit("7x0zd867", value=value)
    tnbzoc5e: int  # size
    ora1368t: str  # modified
    qttidvz5: bool  # hidden
    iqh17pvk: str  # original


class Listing(Resource):
    """Listing — `tl.so.tilo.file-explorer.Listing`."""
    __surface__ = "so.tilo.file-explorer@32a9b53c26850c65"
    uzztpcqp: str  # path
    @property
    def _9ilrk28j(self) -> list[Entry]:
        """entries (`9ilrk28j` on the wire)."""
        return self.fields["9ilrk28j"]

    @_9ilrk28j.setter
    def _9ilrk28j(self, value: list[Entry]) -> None:
        self.edit("9ilrk28j", value=value)
    qwo5qxtp: str  # error
    yflohkj0: bool  # read
    fzgm7n2x: str  # watch


class Place(Resource):
    """Place — `tl.so.tilo.file-explorer.Place`."""
    __surface__ = "so.tilo.file-explorer@32a9b53c26850c65"
    kcy1i0qo: str  # label
    z0dyhfw9: str  # path
    @property
    def _2v9tcdo2(self) -> str:
        """group (`2v9tcdo2` on the wire)."""
        return self.fields["2v9tcdo2"]

    @_2v9tcdo2.setter
    def _2v9tcdo2(self, value: str) -> None:
        self.edit("2v9tcdo2", value=value)


class Favorite(Resource):
    """Favorite — `tl.so.tilo.file-explorer.Favorite`."""
    __surface__ = "so.tilo.file-explorer@32a9b53c26850c65"
    m5mpzvqt: str  # place


class Recent(Resource):
    """Recent — `tl.so.tilo.file-explorer.Recent`."""
    __surface__ = "so.tilo.file-explorer@32a9b53c26850c65"
    v8w5s5s2: str  # name
    ddeyygde: str  # path
    flcdg3ss: str  # opened


class Preview(Resource):
    """Preview — `tl.so.tilo.file-explorer.Preview`."""
    __surface__ = "so.tilo.file-explorer@32a9b53c26850c65"
    @property
    def _3knlafaw(self) -> str:
        """path (`3knlafaw` on the wire)."""
        return self.fields["3knlafaw"]

    @_3knlafaw.setter
    def _3knlafaw(self, value: str) -> None:
        self.edit("3knlafaw", value=value)
    @property
    def _2gbzq5wu(self) -> str:
        """modified (`2gbzq5wu` on the wire)."""
        return self.fields["2gbzq5wu"]

    @_2gbzq5wu.setter
    def _2gbzq5wu(self, value: str) -> None:
        self.edit("2gbzq5wu", value=value)
    e52oeizb: Any | None  # file


class Destination(Resource):
    """Destination — `tl.so.tilo.file-explorer.Destination`."""
    __surface__ = "so.tilo.file-explorer@32a9b53c26850c65"
    @property
    def _3wtmjgc5(self) -> str:
        """name (`3wtmjgc5` on the wire)."""
        return self.fields["3wtmjgc5"]

    @_3wtmjgc5.setter
    def _3wtmjgc5(self, value: str) -> None:
        self.edit("3wtmjgc5", value=value)
    gpo7gdkp: str  # path


class Transfer(Resource):
    """Transfer — `tl.so.tilo.file-explorer.Transfer`."""
    __surface__ = "so.tilo.file-explorer@32a9b53c26850c65"
    holiaqgm: str  # job
    dtg3bicb: str  # label
    b4sboihw: int  # done
    yqeek7os: int  # total
    o5vail2q: str  # current
    gdzzzq6x: str  # failed

    def transfer_moved(self, *, j: Any) -> None:
        """How far the transfer `t` has got, as the device records each move."""
        return self.action("transfer_moved", j=j)

    def transfer_moved_many(self, items: list[TransferMovedArgs]) -> list[dict[str, Any]]:
        """Runs `transfer_moved` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("transfer_moved", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"transfer_moved", "transfer_moved_many"})


class FileFacts(Resource):
    """FileFacts — `tl.so.tilo.file-explorer.FileFacts`."""
    __surface__ = "so.tilo.file-explorer@32a9b53c26850c65"
    foqi2yof: int  # width
    sg7ksmy3: int  # height
    opvq63ti: int  # duration
    wzwzsjca: int  # pages
    nea4pran: str  # codec
    nyczknd7: str  # container


class TransferAsk(Resource):
    """TransferAsk — `tl.so.tilo.file-explorer.TransferAsk`."""
    __surface__ = "so.tilo.file-explorer@32a9b53c26850c65"
    wsdpc7wm: list[str]  # paths
    @property
    def _3s3dhes2(self) -> str:
        """folder (`3s3dhes2` on the wire)."""
        return self.fields["3s3dhes2"]

    @_3s3dhes2.setter
    def _3s3dhes2(self, value: str) -> None:
        self.edit("3s3dhes2", value=value)
    @property
    def _3ebinaph(self) -> bool:
        """moving (`3ebinaph` on the wire)."""
        return self.fields["3ebinaph"]

    @_3ebinaph.setter
    def _3ebinaph(self, value: bool) -> None:
        self.edit("3ebinaph", value=value)


class TransferState(Resource):
    """TransferState — `tl.so.tilo.file-explorer.TransferState`."""
    __surface__ = "so.tilo.file-explorer@32a9b53c26850c65"
    xy4h2hjf: int  # done
    xqn7tfzw: int  # total
    mfmf4wlj: str  # current


class Session(Resource):
    """Session — `tl.so.tilo.file-explorer.Session`."""
    __surface__ = "so.tilo.file-explorer@32a9b53c26850c65"
    zxu7qmx7: str  # here
    roylouv1: SortBy  # sort
    hm8xkfti: bool  # ascending
    gkgwx8bk: bool  # hidden
    @property
    def _31bypi4c(self) -> bool:
        """grid (`31bypi4c` on the wire)."""
        return self.fields["31bypi4c"]

    @_31bypi4c.setter
    def _31bypi4c(self, value: bool) -> None:
        self.edit("31bypi4c", value=value)
    @property
    def _2myooacl(self) -> int:
        """zoom (`2myooacl` on the wire)."""
        return self.fields["2myooacl"]

    @_2myooacl.setter
    def _2myooacl(self, value: int) -> None:
        self.edit("2myooacl", value=value)
    @property
    def _9cdifavr(self) -> list[str]:
        """went (`9cdifavr` on the wire)."""
        return self.fields["9cdifavr"]

    @_9cdifavr.setter
    def _9cdifavr(self, value: list[str]) -> None:
        self.edit("9cdifavr", value=value)
    @property
    def _4tiugype(self) -> list[str]:
        """ahead (`4tiugype` on the wire)."""
        return self.fields["4tiugype"]

    @_4tiugype.setter
    def _4tiugype(self, value: list[str]) -> None:
        self.edit("4tiugype", value=value)
    @property
    def _6twkbcct(self) -> list[str]:
        """carrying (`6twkbcct` on the wire)."""
        return self.fields["6twkbcct"]

    @_6twkbcct.setter
    def _6twkbcct(self, value: list[str]) -> None:
        self.edit("6twkbcct", value=value)
    @property
    def _5y3gacnc(self) -> bool:
        """cutting (`5y3gacnc` on the wire)."""
        return self.fields["5y3gacnc"]

    @_5y3gacnc.setter
    def _5y3gacnc(self, value: bool) -> None:
        self.edit("5y3gacnc", value=value)
    @property
    def _9psytdam(self) -> list[Entry[Listing]]:
        """listings (`9psytdam` on the wire)."""
        return self.fields["9psytdam"]

    @_9psytdam.setter
    def _9psytdam(self, value: list[Entry[Listing]]) -> None:
        self.edit("9psytdam", value=value)
    @property
    def _2kuloiux(self) -> list[Entry[Preview]]:
        """previews (`2kuloiux` on the wire)."""
        return self.fields["2kuloiux"]

    @_2kuloiux.setter
    def _2kuloiux(self, value: list[Entry[Preview]]) -> None:
        self.edit("2kuloiux", value=value)
    @property
    def _8kzpf3es(self) -> Any | None:
        """preview (`8kzpf3es` on the wire)."""
        return self.fields["8kzpf3es"]

    @_8kzpf3es.setter
    def _8kzpf3es(self, value: Any | None) -> None:
        self.edit("8kzpf3es", value=value)
    omzv6cq5: str  # previewed
    l1bciwln: str  # words
    @property
    def _27yiejjv(self) -> FileFacts | None:
        """facts (`27yiejjv` on the wire)."""
        return self.fields["27yiejjv"]

    @_27yiejjv.setter
    def _27yiejjv(self, value: FileFacts | None) -> None:
        self.edit("27yiejjv", value=value)
    wduoe4ej: list[str]  # chosen
    zgzedmq4: str  # selected
    kljwn54u: list[str]  # templates
    uswipaet: str  # opening
    v4v6h4ut: list[Any]  # openers
    fpb2xkum: str  # notice
    @property
    def _3jhnrnyu(self) -> list[Entry[Transfer]]:
        """transfers (`3jhnrnyu` on the wire)."""
        return self.fields["3jhnrnyu"]

    @_3jhnrnyu.setter
    def _3jhnrnyu(self, value: list[Entry[Transfer]]) -> None:
        self.edit("3jhnrnyu", value=value)


class Files(Resource):
    """Files — `tl.so.tilo.file-explorer.Files`."""
    __surface__ = "so.tilo.file-explorer@32a9b53c26850c65"
    @property
    def _7dfrn16h(self) -> list[Entry[Session]]:
        """sessions (`7dfrn16h` on the wire)."""
        return self.fields["7dfrn16h"]

    @_7dfrn16h.setter
    def _7dfrn16h(self, value: list[Entry[Session]]) -> None:
        self.edit("7dfrn16h", value=value)
    fuihvmu6: list[Entry[Place]]  # places
    j9xycg06: list[Entry[Favorite]]  # favorites
    @property
    def _60ufpbms(self) -> list[Entry[Recent]]:
        """recent (`60ufpbms` on the wire)."""
        return self.fields["60ufpbms"]

    @_60ufpbms.setter
    def _60ufpbms(self, value: list[Entry[Recent]]) -> None:
        self.edit("60ufpbms", value=value)

    def read_listing(self,
        *,
        place: str | None = None,
    ) -> None:
        """Reads the place `place` into this window's listing for it: a folder's entries, this device's trash, or nothing for the places this app keeps itself."""
        args: dict[str, Any] = {}
        if place is not None:
            args["place"] = place
        return self.action("read_listing", **args)

    def read_listing_many(self, items: list[ReadListingArgs]) -> list[dict[str, Any]]:
        """Runs `read_listing` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("read_listing", items)

    def changed(self) -> None:
        """Runs when a watched folder changes: reads the folder shown again."""
        return self.action("changed")

    def changed_many(self, items: list[ChangedArgs]) -> list[dict[str, Any]]:
        """Runs `changed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("changed", items)

    def preview_step(self) -> None:
        """Makes the previews of a few pictures in the folder shown that have none yet, as the window shows it: a folder of photographs fills with their pictures while the person looks at it, without the window waiting for every one when the folder is read."""
        return self.action("preview_step")

    def preview_step_many(self, items: list[PreviewStepArgs]) -> list[dict[str, Any]]:
        """Runs `preview_step` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("preview_step", items)

    def go(self, *, place: str) -> None:
        """Goes to the place `place`, reading it: a folder, the trash, the recent files or the apps."""
        return self.action("go", place=place)

    def go_many(self, items: list[GoArgs]) -> list[dict[str, Any]]:
        """Runs `go` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("go", items)

    def back(self) -> None:
        """Goes back to the place the person came from."""
        return self.action("back")

    def back_many(self, items: list[BackArgs]) -> list[dict[str, Any]]:
        """Runs `back` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("back", items)

    def forward(self) -> None:
        """Goes forward again to the place the person left."""
        return self.action("forward")

    def forward_many(self, items: list[ForwardArgs]) -> list[dict[str, Any]]:
        """Runs `forward` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("forward", items)

    def up(self) -> None:
        """Goes to the folder above the one shown."""
        return self.action("up")

    def up_many(self, items: list[UpArgs]) -> list[dict[str, Any]]:
        """Runs `up` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("up", items)

    def show(self, *, path: str) -> None:
        """Shows `path` in this window: the folder itself when it is one, and the folder that holds it with the entry chosen when it is a file. It is how another app sends a person to a file it made, and how an agent or a link brings them to one, so the person reads where it stands in its folder."""
        return self.action("show", path=path)

    def show_many(self, items: list[ShowArgs]) -> list[dict[str, Any]]:
        """Runs `show` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show", items)

    def extract_into(self,
        *,
        path: str,
        folder: str,
    ) -> None:
        """Extracts the archive at `path` into `folder`, into a folder named after it there: what the rows that offer a folder to extract into ask."""
        return self.action("extract_into", path=path, folder=folder)

    def extract_into_many(self, items: list[ExtractIntoArgs]) -> list[dict[str, Any]]:
        """Runs `extract_into` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("extract_into", items)

    def move_into(self, *, folder: str) -> None:
        """Moves the entries the person chose into `folder`, keeping a taken name both."""
        return self.action("move_into", folder=folder)

    def move_into_many(self, items: list[MoveIntoArgs]) -> list[dict[str, Any]]:
        """Runs `move_into` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_into", items)

    def drop_into(self,
        *,
        paths: list[str],
        folder: str,
    ) -> None:
        """Moves the entries the person dragged onto the folder `folder`, by their `paths`, as 'Move to…' does, keeping a taken name both. A folder let go on itself, inside itself or on the folder its entries already stand in moves nothing."""
        return self.action("drop_into", paths=paths, folder=folder)

    def drop_into_many(self, items: list[DropIntoArgs]) -> list[dict[str, Any]]:
        """Runs `drop_into` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("drop_into", items)

    def copy_into(self, *, folder: str) -> None:
        """Copies the entries the person chose into `folder`, keeping a taken name both."""
        return self.action("copy_into", folder=folder)

    def copy_into_many(self, items: list[CopyIntoArgs]) -> list[dict[str, Any]]:
        """Runs `copy_into` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("copy_into", items)

    def put_into(self,
        *,
        paths: list[str],
        folder: str,
        moving: bool,
    ) -> None:
        """Puts the entries at `paths` into `folder`, moving them or copying them, as the device's own work: the window shows how far it has got and can stop it. What arrives under a name the folder already holds lands as “name (2)”, so nothing is replaced."""
        return self.action("put_into", paths=paths, folder=folder, moving=moving)

    def put_into_many(self, items: list[PutIntoArgs]) -> list[dict[str, Any]]:
        """Runs `put_into` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("put_into", items)

    def transfer_ended(self,
        *,
        t: Transfer,
        j: Any,
    ) -> None:
        """The transfer `t` ended: one that finished leaves the window and the folder is read again; one that failed stays, saying why, until it is dismissed."""
        return self.action("transfer_ended", t=t, j=j)

    def transfer_ended_many(self, items: list[TransferEndedArgs]) -> list[dict[str, Any]]:
        """Runs `transfer_ended` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("transfer_ended", items)

    def stop_transfer(self, *, transfer: Transfer) -> None:
        """Stops the transfer `transfer`: the entry it was on is taken away, and what it finished stays."""
        return self.action("stop_transfer", transfer=transfer)

    def dismiss_transfer(self, *, transfer: Transfer) -> None:
        """Takes a transfer that stopped short out of the window."""
        return self.action("dismiss_transfer", transfer=transfer)

    def dismiss_transfer_many(self, items: list[DismissTransferArgs]) -> list[dict[str, Any]]:
        """Runs `dismiss_transfer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("dismiss_transfer", items)

    def send_nearby(self, *, nearby: str) -> None:
        """Sends the entries the person chose to the nearby device `nearby`, as Nearby Send offers it."""
        return self.action("send_nearby", nearby=nearby)

    def dismiss_notice(self) -> None:
        """Dismisses what the window last said went wrong."""
        return self.action("dismiss_notice")

    def dismiss_notice_many(self, items: list[DismissNoticeArgs]) -> list[dict[str, Any]]:
        """Runs `dismiss_notice` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("dismiss_notice", items)

    # the app's `refresh` action is the SDK's own: run it with `action("refresh")`.
    def sort_by(self, *, by: SortBy) -> None:
        """Sorts the folder shown by `by`, the way `ascending` says."""
        return self.action("sort_by", by=by)

    def sort_by_many(self, items: list[SortByArgs]) -> list[dict[str, Any]]:
        """Runs `sort_by` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("sort_by", items)

    def ascending(self, *, first: bool) -> None:
        """Sorts the folder shown oldest or smallest first, or the other way."""
        return self.action("ascending", first=first)

    def ascending_many(self, items: list[AscendingArgs]) -> list[dict[str, Any]]:
        """Runs `ascending` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("ascending", items)

    def show_grid(self, *, grid: bool) -> None:
        """Draws the folder as a grid of cards, or as rows."""
        return self.action("show_grid", grid=grid)

    def show_grid_many(self, items: list[ShowGridArgs]) -> list[dict[str, Any]]:
        """Runs `show_grid` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show_grid", items)

    def choose_view(self, *, i: int) -> None:
        """Draws the folder the way the segment at place `i` says: the list at 0, the grid at 1."""
        return self.action("choose_view", i=i)

    def choose_view_many(self, items: list[ChooseViewArgs]) -> list[dict[str, Any]]:
        """Runs `choose_view` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("choose_view", items)

    def toggle_hidden(self, *, hidden: bool) -> None:
        """Shows or hides the names that start with a dot."""
        return self.action("toggle_hidden", hidden=hidden)

    def toggle_hidden_many(self, items: list[ToggleHiddenArgs]) -> list[dict[str, Any]]:
        """Runs `toggle_hidden` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("toggle_hidden", items)

    def set_zoom(self, *, step: int) -> None:
        """Draws the entries at `step`: 0 smaller, 1 regular, 2 larger, as the two keys step it."""
        return self.action("set_zoom", step=step)

    def set_zoom_many(self, items: list[SetZoomArgs]) -> list[dict[str, Any]]:
        """Runs `set_zoom` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_zoom", items)

    def zoom_in(self) -> None:
        """Draws the entries one step larger."""
        return self.action("zoom_in")

    def zoom_in_many(self, items: list[ZoomInArgs]) -> list[dict[str, Any]]:
        """Runs `zoom_in` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("zoom_in", items)

    def zoom_out(self) -> None:
        """Draws the entries one step smaller."""
        return self.action("zoom_out")

    def zoom_out_many(self, items: list[ZoomOutArgs]) -> list[dict[str, Any]]:
        """Runs `zoom_out` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("zoom_out", items)

    def open_entry(self, *, path: str) -> None:
        """Opens the entry `e`: a folder in this window, a file in the app that opens its kind."""
        return self.action("open_entry", path=path)

    def open_entry_many(self, items: list[OpenEntryArgs]) -> list[dict[str, Any]]:
        """Runs `open_entry` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("open_entry", items)

    def find_openers(self, *, path: str) -> None:
        """Finds the apps that open the entry at `path`, for the person to choose one."""
        return self.action("find_openers", path=path)

    def find_openers_many(self, items: list[FindOpenersArgs]) -> list[dict[str, Any]]:
        """Runs `find_openers` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("find_openers", items)

    def open_with(self, *, chosen: str) -> None:
        """Opens the entry the person asked to open with in `chosen`, one of the apps offered."""
        return self.action("open_with", chosen=chosen)

    def open_with_many(self, items: list[OpenWithArgs]) -> list[dict[str, Any]]:
        """Runs `open_with` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("open_with", items)

    def close_openers(self) -> None:
        """Puts away the apps offered for the entry the person asked to open with."""
        return self.action("close_openers")

    def close_openers_many(self, items: list[CloseOpenersArgs]) -> list[dict[str, Any]]:
        """Runs `close_openers` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("close_openers", items)

    def open_terminal(self, *, folder: str) -> None:
        """Opens a terminal in the folder at `folder`."""
        return self.action("open_terminal", folder=folder)

    def open_terminal_many(self, items: list[OpenTerminalArgs]) -> list[dict[str, Any]]:
        """Runs `open_terminal` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("open_terminal", items)

    def compress_entry(self,
        *,
        path: str,
        zip: bool,
    ) -> None:
        """Packs the entry at `path` into a new archive beside it, named after it: `zip` picks a ZIP, and TAR.ZST otherwise."""
        return self.action("compress_entry", path=path, zip=zip)

    def compress_entry_many(self, items: list[CompressEntryArgs]) -> list[dict[str, Any]]:
        """Runs `compress_entry` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("compress_entry", items)

    def extract_entry(self, *, path: str) -> None:
        """Extracts the archive at `path` into a new folder beside it, named after it."""
        return self.action("extract_entry", path=path)

    def extract_entry_many(self, items: list[ExtractEntryArgs]) -> list[dict[str, Any]]:
        """Runs `extract_entry` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("extract_entry", items)

    def touch_entry(self, *, path: str) -> None:
        """A press on the entry at `path`: a folder opens as a person presses it once, and a file is read into the inspector beside the list."""
        return self.action("touch_entry", path=path)

    def touch_entry_many(self, items: list[TouchEntryArgs]) -> list[dict[str, Any]]:
        """Runs `touch_entry` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("touch_entry", items)

    def choose(self, *, paths: list[str]) -> None:
        """The entries the list reports as chosen, in the order they stand there."""
        return self.action("choose", paths=paths)

    def choose_many(self, items: list[ChooseArgs]) -> list[dict[str, Any]]:
        """Runs `choose` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("choose", items)

    def select_all(self) -> None:
        """Chooses every entry the folder shown places: what a press on 'Select all' does."""
        return self.action("select_all")

    def select_all_many(self, items: list[SelectAllArgs]) -> list[dict[str, Any]]:
        """Runs `select_all` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("select_all", items)

    def clear_selection(self) -> None:
        """Chooses nothing."""
        return self.action("clear_selection")

    def clear_selection_many(self, items: list[ClearSelectionArgs]) -> list[dict[str, Any]]:
        """Runs `clear_selection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_selection", items)

    def note_recent(self,
        *,
        name: str,
        path: str,
    ) -> None:
        """Notes that the file `path` was opened, so it stands under Recent."""
        return self.action("note_recent", name=name, path=path)

    def note_recent_many(self, items: list[NoteRecentArgs]) -> list[dict[str, Any]]:
        """Runs `note_recent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("note_recent", items)

    def new_folder(self,
        *,
        name: str | None = None,
    ) -> None:
        """Makes a folder named `name` in the place shown."""
        args: dict[str, Any] = {}
        if name is not None:
            args["name"] = name
        return self.action("new_folder", **args)

    def new_folder_many(self, items: list[NewFolderArgs]) -> list[dict[str, Any]]:
        """Runs `new_folder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("new_folder", items)

    def start_new_file(self) -> None:
        """Reads the person's `~/Templates`, so naming a new file can offer them."""
        return self.action("start_new_file")

    def start_new_file_many(self, items: list[StartNewFileArgs]) -> list[dict[str, Any]]:
        """Runs `start_new_file` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("start_new_file", items)

    def new_from_template(self,
        *,
        template: str,
        typed: str,
    ) -> None:
        """Copies the template `template` into the place shown under `typed`'s name, or the template's."""
        return self.action("new_from_template", template=template, typed=typed)

    def new_from_template_many(self, items: list[NewFromTemplateArgs]) -> list[dict[str, Any]]:
        """Runs `new_from_template` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("new_from_template", items)

    def new_file(self,
        *,
        name: str | None = None,
    ) -> None:
        """Makes a text file named `name` in the place shown, for the person to fill."""
        args: dict[str, Any] = {}
        if name is not None:
            args["name"] = name
        return self.action("new_file", **args)

    def new_file_many(self, items: list[NewFileArgs]) -> list[dict[str, Any]]:
        """Runs `new_file` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("new_file", items)

    def import_dropped(self, *, files: list[Any]) -> None:
        """Writes the files a person dropped on the window into the folder shown, each under its own name; a name the folder already holds is kept both, so nothing is replaced."""
        return self.action("import_dropped", files=files)

    def import_dropped_many(self, items: list[ImportDroppedArgs]) -> list[dict[str, Any]]:
        """Runs `import_dropped` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import_dropped", items)

    def open_drive(self, *, drive_id: str) -> None:
        """Opens the drive `drive` and goes to where it is open, so a removable disk is a place like any other folder. A drive that still has to open shows as such until the device says where it is."""
        return self.action("open_drive", drive_id=drive_id)

    def rename_entry(self,
        *,
        path: str,
        name: str,
    ) -> None:
        """Renames the entry at `path` to `name` in its own folder."""
        return self.action("rename_entry", path=path, name=name)

    def rename_entry_many(self, items: list[RenameEntryArgs]) -> list[dict[str, Any]]:
        """Runs `rename_entry` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename_entry", items)

    def trash_entries(self, *, paths: list[str]) -> None:
        """Moves the entries at `paths` to this device's trash."""
        return self.action("trash_entries", paths=paths)

    def trash_entries_many(self, items: list[TrashEntriesArgs]) -> list[dict[str, Any]]:
        """Runs `trash_entries` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("trash_entries", items)

    def restore_entry(self, *, name: str) -> None:
        """Restores the trashed entry `name` to where it came from."""
        return self.action("restore_entry", name=name)

    def restore_entry_many(self, items: list[RestoreEntryArgs]) -> list[dict[str, Any]]:
        """Runs `restore_entry` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("restore_entry", items)

    def delete_permanently(self, *, path: str) -> None:
        """Removes the entry at `path` for good."""
        return self.action("delete_permanently", path=path)

    def delete_permanently_many(self, items: list[DeletePermanentlyArgs]) -> list[dict[str, Any]]:
        """Runs `delete_permanently` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_permanently", items)

    def empty_trash(self) -> None:
        """Empties this device's trash: every entry, and the receipt that would put it back."""
        return self.action("empty_trash")

    def empty_trash_many(self, items: list[EmptyTrashArgs]) -> list[dict[str, Any]]:
        """Runs `empty_trash` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("empty_trash", items)

    def carry(self,
        *,
        paths: list[str],
        cut: bool,
    ) -> None:
        """Remembers the entries at `paths` as what a paste copies; `cut` makes it a move."""
        return self.action("carry", paths=paths, cut=cut)

    def carry_many(self, items: list[CarryArgs]) -> list[dict[str, Any]]:
        """Runs `carry` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("carry", items)

    def carry_selection(self, *, cut: bool) -> None:
        """Remembers the entries the person chose as what a paste copies."""
        return self.action("carry_selection", cut=cut)

    def carry_selection_many(self, items: list[CarrySelectionArgs]) -> list[dict[str, Any]]:
        """Runs `carry_selection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("carry_selection", items)

    def compress_selection(self, *, zip: bool) -> None:
        """Packs the entries the person chose into one archive beside them: `zip` picks a ZIP, and TAR.ZST otherwise. The archive is named after the entry alone, or after its folder when several go in."""
        return self.action("compress_selection", zip=zip)

    def compress_selection_many(self, items: list[CompressSelectionArgs]) -> list[dict[str, Any]]:
        """Runs `compress_selection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("compress_selection", items)

    def paste(self) -> None:
        """Pastes what the person carries into the place shown, as the device's own work that says how far it has got. A name the folder already holds is kept both: what arrives lands as “name (2)”, so nothing is overwritten."""
        return self.action("paste")

    def paste_many(self, items: list[PasteArgs]) -> list[dict[str, Any]]:
        """Runs `paste` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("paste", items)

    def free_name(self,
        *,
        folder: str,
        name: str,
    ) -> str:
        """A name `folder` does not hold yet, for an entry that would land as `name`: the name itself, or “name (2)”, “name (3)” before the extension, so a paste or a drop never replaces a file."""
        return self.action("free_name", folder=folder, name=name)

    def key_on(self,
        *,
        focused: str,
        key: str,
    ) -> None:
        """A key pressed on the row `focused`: what Files does with a copy, a cut or a delete over the entries the person chose, or over that row when it is not among them."""
        return self.action("key_on", focused=focused, key=key)

    def key_on_many(self, items: list[KeyOnArgs]) -> list[dict[str, Any]]:
        """Runs `key_on` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("key_on", items)

    def carry_row(self,
        *,
        path: str,
        cut: bool,
    ) -> None:
        """Remembers what a copy from the row at `path` takes: the entries the person chose when that row is one of them, and the row alone otherwise."""
        return self.action("carry_row", path=path, cut=cut)

    def carry_row_many(self, items: list[CarryRowArgs]) -> list[dict[str, Any]]:
        """Runs `carry_row` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("carry_row", items)

    def trash_row(self, *, path: str) -> None:
        """Moves to Trash what a delete from the row at `path` takes: the entries the person chose when that row is one of them, and the row alone otherwise."""
        return self.action("trash_row", path=path)

    def trash_row_many(self, items: list[TrashRowArgs]) -> list[dict[str, Any]]:
        """Runs `trash_row` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("trash_row", items)

    def add_favorite(self, *, place: str) -> None:
        """Marks the place `place` in the sidebar."""
        return self.action("add_favorite", place=place)

    def add_favorite_many(self, items: list[AddFavoriteArgs]) -> list[dict[str, Any]]:
        """Runs `add_favorite` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_favorite", items)

    def remove_favorite(self, *, place: str) -> None:
        """Takes the place `place` out of the sidebar's favorites."""
        return self.action("remove_favorite", place=place)

    def remove_favorite_many(self, items: list[RemoveFavoriteArgs]) -> list[dict[str, Any]]:
        """Runs `remove_favorite` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_favorite", items)

    def inspect(self, *, path: str) -> None:
        """Reads what the chosen entry is, so the inspector says its size, its kind and its path, and shows a picture file's picture or a text file's first words."""
        return self.action("inspect", path=path)

    def inspect_many(self, items: list[InspectArgs]) -> list[dict[str, Any]]:
        """Runs `inspect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("inspect", items)

    def name_it(self,
        *,
        naming: str,
        typed: str,
    ) -> None:
        """Does what the name asked of the person is for: makes the folder or the file, or renames the entry."""
        return self.action("name_it", naming=naming, typed=typed)

    def name_it_many(self, items: list[NameItArgs]) -> list[dict[str, Any]]:
        """Runs `name_it` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("name_it", items)

    def open_app(self, *, app_key: str) -> None:
        """Opens the app `a`, as pressing its row does."""
        return self.action("open_app", app_key=app_key)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"read_listing", "read_listing_many", "changed", "changed_many", "preview_step", "preview_step_many", "go", "go_many", "back", "back_many", "forward", "forward_many", "up", "up_many", "show", "show_many", "extract_into", "extract_into_many", "move_into", "move_into_many", "drop_into", "drop_into_many", "copy_into", "copy_into_many", "put_into", "put_into_many", "transfer_ended", "transfer_ended_many", "stop_transfer", "dismiss_transfer", "dismiss_transfer_many", "send_nearby", "dismiss_notice", "dismiss_notice_many", "sort_by", "sort_by_many", "ascending", "ascending_many", "show_grid", "show_grid_many", "choose_view", "choose_view_many", "toggle_hidden", "toggle_hidden_many", "set_zoom", "set_zoom_many", "zoom_in", "zoom_in_many", "zoom_out", "zoom_out_many", "open_entry", "open_entry_many", "find_openers", "find_openers_many", "open_with", "open_with_many", "close_openers", "close_openers_many", "open_terminal", "open_terminal_many", "compress_entry", "compress_entry_many", "extract_entry", "extract_entry_many", "touch_entry", "touch_entry_many", "choose", "choose_many", "select_all", "select_all_many", "clear_selection", "clear_selection_many", "note_recent", "note_recent_many", "new_folder", "new_folder_many", "start_new_file", "start_new_file_many", "new_from_template", "new_from_template_many", "new_file", "new_file_many", "import_dropped", "import_dropped_many", "open_drive", "rename_entry", "rename_entry_many", "trash_entries", "trash_entries_many", "restore_entry", "restore_entry_many", "delete_permanently", "delete_permanently_many", "empty_trash", "empty_trash_many", "carry", "carry_many", "carry_selection", "carry_selection_many", "compress_selection", "compress_selection_many", "paste", "paste_many", "free_name", "key_on", "key_on_many", "carry_row", "carry_row_many", "trash_row", "trash_row_many", "add_favorite", "add_favorite_many", "remove_favorite", "remove_favorite_many", "inspect", "inspect_many", "name_it", "name_it_many", "open_app"})


class Crumb(Resource):
    """Crumb — `tl.so.tilo.file-explorer.Crumb`."""
    __surface__ = "so.tilo.file-explorer@32a9b53c26850c65"
    lbeg1rhd: str  # label
    sv3ucdrd: str  # path


class FileExplorer(Resource):
    """Files — `tl.so.tilo.file-explorer`."""
    __surface__ = "so.tilo.file-explorer@32a9b53c26850c65"

