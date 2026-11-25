"""Types for Backups (`so.tilo.backups`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `backups/<resource>`, read with
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
    "Health",
    "Kept",
    "Taken",
    "Cadence",
    "Origin",
    "Notice",
    "Entry",
    "Folded",
    "Snapshot",
    "Policy",
    "Place",
    "Backups",
    "CatalogAsk",
    "BackupAsk",
    "SnapshotAsk",
    "RestoreAsk",
    "WorkDetail",
    "Checked",
    "TiloBackups",
    "KeepCatalogArgs",
    "BackUpNowArgs",
    "VerifyArgs",
    "AskRestoreArgs",
    "BeginRestoreArgs",
    "RestoreNowArgs",
    "StopWorkArgs",
    "WorkMovedArgs",
    "WorkEndedArgs",
    "OpenSnapshotArgs",
    "SetOrderArgs",
    "ShowPlaceArgs",
    "KeepMoreArgs",
    "KeepFewerArgs",
    "DismissArgs",
    "ChooseSourceArgs",
    "ChooseDestinationArgs",
    "GoArgs",
    "GoUpArgs",
    "PickArgs",
    "CancelPickArgs",
    "ReadPickerArgs",
    "StartWorkArgs",
    "CloseSnapshotArgs",
    "AllowOverwriteArgs",
    "FoldFolderArgs",
]

Health = Literal["complete", "verifying", "failed"]
"""Whether a stored snapshot is whole, being checked or known broken."""

Kept = Literal["hourly", "daily", "weekly", "monthly", "kept", "unknown"]
"""The retention class a snapshot counts in."""

Taken = Literal["automatic", "manual", "unknown"]
"""Was a snapshot taken by the schedule or by hand?"""

Cadence = Literal["on_demand", "hourly", "daily", "weekly"]
"""When a source backs itself up."""

Origin = Literal["home", "system", "workspace", "custom"]
"""What a source is, as much as its path says without naming it."""

KeepCatalogArgs = TypedDict("KeepCatalogArgs", {
    "answer": "str",
})
"""The arguments of `keep_catalog`."""

BackUpNowArgs = TypedDict("BackUpNowArgs", {
})
"""The arguments of `back_up_now`."""

VerifyArgs = TypedDict("VerifyArgs", {
    "s": "Snapshot",
})
"""The arguments of `verify`."""

AskRestoreArgs = TypedDict("AskRestoreArgs", {
    "s": "Snapshot",
})
"""The arguments of `ask_restore`."""

BeginRestoreArgs = TypedDict("BeginRestoreArgs", {
})
"""The arguments of `begin_restore`."""

RestoreNowArgs = TypedDict("RestoreNowArgs", {
    "s": "Snapshot",
})
"""The arguments of `restore_now`."""

StopWorkArgs = TypedDict("StopWorkArgs", {
})
"""The arguments of `stop_work`."""

WorkMovedArgs = TypedDict("WorkMovedArgs", {
    "j": "Any",
})
"""The arguments of `work_moved`."""

WorkEndedArgs = TypedDict("WorkEndedArgs", {
    "kind": "str",
    "j": "Any",
})
"""The arguments of `work_ended`."""

OpenSnapshotArgs = TypedDict("OpenSnapshotArgs", {
    "s": "Snapshot",
})
"""The arguments of `open_snapshot`."""

SetOrderArgs = TypedDict("SetOrderArgs", {
    "newest_first": "bool",
})
"""The arguments of `set_order`."""

ShowPlaceArgs = TypedDict("ShowPlaceArgs", {
    "place": "str",
})
"""The arguments of `show_place`."""

KeepMoreArgs = TypedDict("KeepMoreArgs", {
    "which": "str",
})
"""The arguments of `keep_more`."""

KeepFewerArgs = TypedDict("KeepFewerArgs", {
    "which": "str",
})
"""The arguments of `keep_fewer`."""

DismissArgs = TypedDict("DismissArgs", {
})
"""The arguments of `dismiss`."""

ChooseSourceArgs = TypedDict("ChooseSourceArgs", {
})
"""The arguments of `choose_source`."""

ChooseDestinationArgs = TypedDict("ChooseDestinationArgs", {
})
"""The arguments of `choose_destination`."""

GoArgs = TypedDict("GoArgs", {
    "path": "str",
})
"""The arguments of `go`."""

GoUpArgs = TypedDict("GoUpArgs", {
})
"""The arguments of `go_up`."""

PickArgs = TypedDict("PickArgs", {
})
"""The arguments of `pick`."""

CancelPickArgs = TypedDict("CancelPickArgs", {
})
"""The arguments of `cancel_pick`."""

ReadPickerArgs = TypedDict("ReadPickerArgs", {
})
"""The arguments of `read_picker`."""

StartWorkArgs = TypedDict("StartWorkArgs", {
    "kind": "str",
    "ask": "str",
})
"""The arguments of `start_work`."""

CloseSnapshotArgs = TypedDict("CloseSnapshotArgs", {
})
"""The arguments of `close_snapshot`."""

AllowOverwriteArgs = TypedDict("AllowOverwriteArgs", {
})
"""The arguments of `allow_overwrite`."""

FoldFolderArgs = TypedDict("FoldFolderArgs", {
    "s": "Snapshot",
    "path": "str",
    "folded": "bool",
})
"""The arguments of `fold_folder`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.backups@cf21eed351c07025"
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
    """Entry — `tl.so.tilo.backups.Entry`."""
    __surface__ = "so.tilo.backups@cf21eed351c07025"
    cvlieest: str  # name
    dssaufce: str  # path
    c2m5wzf4: int  # depth
    pfl5jyx2: int  # size
    tzyurmps: bool  # folder


class Folded(Resource):
    """Folded — `tl.so.tilo.backups.Folded`."""
    __surface__ = "so.tilo.backups@cf21eed351c07025"
    yp5mxjwc: str  # path


class Snapshot(Resource):
    """Snapshot — `tl.so.tilo.backups.Snapshot`."""
    __surface__ = "so.tilo.backups@cf21eed351c07025"
    dallfwqf: str  # label
    ngqbekfc: int  # at
    hydekdnu: Taken  # taken
    yffctwuv: Kept  # kept
    @property
    def _6kvlgzq4(self) -> Health:
        """health (`6kvlgzq4` on the wire)."""
        return self.fields["6kvlgzq4"]

    @_6kvlgzq4.setter
    def _6kvlgzq4(self, value: Health) -> None:
        self.edit("6kvlgzq4", value=value)
    ljb7imfg: int  # size_bytes
    vcgdjszb: int  # delta_bytes
    yr6zgb2x: int  # files_changed
    cyesgrji: str  # failure
    @property
    def _5nvgiivd(self) -> list[Entry[Entry]]:
        """files (`5nvgiivd` on the wire)."""
        return self.fields["5nvgiivd"]

    @_5nvgiivd.setter
    def _5nvgiivd(self, value: list[Entry[Entry]]) -> None:
        self.edit("5nvgiivd", value=value)
    sikyuvd6: list[Entry[Folded]]  # folded


class Policy(Resource):
    """Policy — `tl.so.tilo.backups.Policy`."""
    __surface__ = "so.tilo.backups@cf21eed351c07025"
    cyugw7lh: int  # hourly
    avot6p6r: int  # daily
    ywba2jzd: int  # weekly
    @property
    def _3miz6dsu(self) -> int:
        """monthly (`3miz6dsu` on the wire)."""
        return self.fields["3miz6dsu"]

    @_3miz6dsu.setter
    def _3miz6dsu(self, value: int) -> None:
        self.edit("3miz6dsu", value=value)


class Place(Resource):
    """Place — `tl.so.tilo.backups.Place`."""
    __surface__ = "so.tilo.backups@cf21eed351c07025"
    dho3qqg3: str  # name
    f3pl3mol: str  # path


class Backups(Resource):
    """Backups — `tl.so.tilo.backups.Backups`."""
    __surface__ = "so.tilo.backups@cf21eed351c07025"
    p2imk5l5: Policy  # policy
    srrmxp5i: str  # source
    @property
    def _6e5yqotr(self) -> str:
        """destination (`6e5yqotr` on the wire)."""
        return self.fields["6e5yqotr"]

    @_6e5yqotr.setter
    def _6e5yqotr(self, value: str) -> None:
        self.edit("6e5yqotr", value=value)
    @property
    def _62wijgsn(self) -> str:
        """source_name (`62wijgsn` on the wire)."""
        return self.fields["62wijgsn"]

    @_62wijgsn.setter
    def _62wijgsn(self, value: str) -> None:
        self.edit("62wijgsn", value=value)
    x6ca2mpx: Origin  # source_origin
    gfsjtev7: Cadence  # source_cadence
    @property
    def _3wbouwis(self) -> bool:
        """source_failing (`3wbouwis` on the wire)."""
        return self.fields["3wbouwis"]

    @_3wbouwis.setter
    def _3wbouwis(self, value: bool) -> None:
        self.edit("3wbouwis", value=value)
    ymfvuwvo: int  # next_run
    v3i3cihq: str  # destination_name
    kqbve5pf: int  # used_bytes
    mdbssite: int  # total_bytes
    kz5xznku: bool  # encrypted
    hlhhrwm3: bool  # destination_failing
    n7gmur3t: list[Entry[Snapshot]]  # snapshots
    ik3j63rv: str  # place
    @property
    def _6fg5mkqa(self) -> bool:
        """newest_first (`6fg5mkqa` on the wire)."""
        return self.fields["6fg5mkqa"]

    @_6fg5mkqa.setter
    def _6fg5mkqa(self, value: bool) -> None:
        self.edit("6fg5mkqa", value=value)
    sxzlbnam: str  # trouble
    yelwker2: str  # failure
    @property
    def _6rkekawk(self) -> str:
        """notice (`6rkekawk` on the wire)."""
        return self.fields["6rkekawk"]

    @_6rkekawk.setter
    def _6rkekawk(self, value: str) -> None:
        self.edit("6rkekawk", value=value)
    bluheyea: str  # job
    wmrb4vzi: str  # working
    v7lhpobw: str  # progress
    xsai3dea: str  # result
    fihmdmmi: int  # subject
    q6dvqz3z: str  # picking
    vlgd4tne: str  # picker
    @property
    def _6nds5bnc(self) -> list[Entry[Place]]:
        """picks (`6nds5bnc` on the wire)."""
        return self.fields["6nds5bnc"]

    @_6nds5bnc.setter
    def _6nds5bnc(self, value: list[Entry[Place]]) -> None:
        self.edit("6nds5bnc", value=value)
    @property
    def _5dqejapv(self) -> str:
        """restore_into (`5dqejapv` on the wire)."""
        return self.fields["5dqejapv"]

    @_5dqejapv.setter
    def _5dqejapv(self, value: str) -> None:
        self.edit("5dqejapv", value=value)
    azcuku5y: bool  # overwrite

    # the app's `refresh` action is the SDK's own: run it with `action("refresh")`.
    def keep_catalog(self, *, answer: str) -> None:
        """Keeps what the device answered about its repository, so the window and an agent read the same snapshots."""
        return self.action("keep_catalog", answer=answer)

    def keep_catalog_many(self, items: list[KeepCatalogArgs]) -> list[dict[str, Any]]:
        """Runs `keep_catalog` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("keep_catalog", items)

    def back_up_now(self) -> None:
        """Backs the folder up now, keeping what the retention policy keeps."""
        return self.action("back_up_now")

    def back_up_now_many(self, items: list[BackUpNowArgs]) -> list[dict[str, Any]]:
        """Runs `back_up_now` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("back_up_now", items)

    def verify(self, *, s: Snapshot) -> None:
        """Checks that every file of a snapshot is still what the snapshot recorded."""
        return self.action("verify", s=s)

    def verify_many(self, items: list[VerifyArgs]) -> list[dict[str, Any]]:
        """Runs `verify` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("verify", items)

    def ask_restore(self, *, s: Snapshot) -> None:
        """Opens the sheet that asks where a snapshot goes back to."""
        return self.action("ask_restore", s=s)

    def ask_restore_many(self, items: list[AskRestoreArgs]) -> list[dict[str, Any]]:
        """Runs `ask_restore` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("ask_restore", items)

    def begin_restore(self) -> None:
        """Starts finding the folder a restore writes into, without running anything yet."""
        return self.action("begin_restore")

    def begin_restore_many(self, items: list[BeginRestoreArgs]) -> list[dict[str, Any]]:
        """Runs `begin_restore` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("begin_restore", items)

    def restore_now(self, *, s: Snapshot) -> None:
        """Restores the snapshot into the folder the person chose."""
        return self.action("restore_now", s=s)

    def restore_now_many(self, items: list[RestoreNowArgs]) -> list[dict[str, Any]]:
        """Runs `restore_now` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("restore_now", items)

    def stop_work(self) -> None:
        """Stops the work under way; what it was writing goes."""
        return self.action("stop_work")

    def stop_work_many(self, items: list[StopWorkArgs]) -> list[dict[str, Any]]:
        """Runs `stop_work` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("stop_work", items)

    def work_moved(self, *, j: Any) -> None:
        """The work moved on: how far it has got, in files and bytes."""
        return self.action("work_moved", j=j)

    def work_moved_many(self, items: list[WorkMovedArgs]) -> list[dict[str, Any]]:
        """Runs `work_moved` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("work_moved", items)

    def work_ended(self,
        *,
        kind: str,
        j: Any,
    ) -> None:
        """The work ended: the repository as it now stands, what a check found, or why it failed."""
        return self.action("work_ended", kind=kind, j=j)

    def work_ended_many(self, items: list[WorkEndedArgs]) -> list[dict[str, Any]]:
        """Runs `work_ended` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("work_ended", items)

    def open_snapshot(self, *, s: Snapshot) -> None:
        """Shows one snapshot's sheet: what it holds and what can be done with it."""
        return self.action("open_snapshot", s=s)

    def open_snapshot_many(self, items: list[OpenSnapshotArgs]) -> list[dict[str, Any]]:
        """Runs `open_snapshot` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("open_snapshot", items)

    def set_order(self, *, newest_first: bool) -> None:
        """Lists the timeline newest first, or oldest first."""
        return self.action("set_order", newest_first=newest_first)

    def set_order_many(self, items: list[SetOrderArgs]) -> list[dict[str, Any]]:
        """Runs `set_order` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_order", items)

    def show_place(self, *, place: str) -> None:
        """Shows one of the sidebar's places."""
        return self.action("show_place", place=place)

    def show_place_many(self, items: list[ShowPlaceArgs]) -> list[dict[str, Any]]:
        """Runs `show_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show_place", items)

    def keep_more(self, *, which: str) -> None:
        """Keeps one more snapshot of a class."""
        return self.action("keep_more", which=which)

    def keep_more_many(self, items: list[KeepMoreArgs]) -> list[dict[str, Any]]:
        """Runs `keep_more` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("keep_more", items)

    def keep_fewer(self, *, which: str) -> None:
        """Keeps one fewer snapshot of a class."""
        return self.action("keep_fewer", which=which)

    def keep_fewer_many(self, items: list[KeepFewerArgs]) -> list[dict[str, Any]]:
        """Runs `keep_fewer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("keep_fewer", items)

    def dismiss(self) -> None:
        """Dismisses the line under the list."""
        return self.action("dismiss")

    def dismiss_many(self, items: list[DismissArgs]) -> list[dict[str, Any]]:
        """Runs `dismiss` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("dismiss", items)

    def choose_source(self) -> None:
        """Asks the person which folder this device protects."""
        return self.action("choose_source")

    def choose_source_many(self, items: list[ChooseSourceArgs]) -> list[dict[str, Any]]:
        """Runs `choose_source` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("choose_source", items)

    def choose_destination(self) -> None:
        """Asks the person where the snapshots go."""
        return self.action("choose_destination")

    def choose_destination_many(self, items: list[ChooseDestinationArgs]) -> list[dict[str, Any]]:
        """Runs `choose_destination` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("choose_destination", items)

    def go(self, *, path: str) -> None:
        """Walks the picker to another folder and shows the folders in it."""
        return self.action("go", path=path)

    def go_many(self, items: list[GoArgs]) -> list[dict[str, Any]]:
        """Runs `go` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("go", items)

    def go_up(self) -> None:
        """Walks the picker up one folder."""
        return self.action("go_up")

    def go_up_many(self, items: list[GoUpArgs]) -> list[dict[str, Any]]:
        """Runs `go_up` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("go_up", items)

    def pick(self) -> None:
        """Takes the folder the picker shows: as what this device protects, where snapshots go, or where a restore writes."""
        return self.action("pick")

    def pick_many(self, items: list[PickArgs]) -> list[dict[str, Any]]:
        """Runs `pick` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("pick", items)

    def cancel_pick(self) -> None:
        """Leaves the picker without changing anything."""
        return self.action("cancel_pick")

    def cancel_pick_many(self, items: list[CancelPickArgs]) -> list[dict[str, Any]]:
        """Runs `cancel_pick` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("cancel_pick", items)

    def read_picker(self) -> None:
        """Reads the folders of the one the picker shows."""
        return self.action("read_picker")

    def read_picker_many(self, items: list[ReadPickerArgs]) -> list[dict[str, Any]]:
        """Runs `read_picker` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("read_picker", items)

    def start_work(self,
        *,
        kind: str,
        ask: str,
    ) -> None:
        """Starts a piece of work and keeps the job the device gave it."""
        return self.action("start_work", kind=kind, ask=ask)

    def start_work_many(self, items: list[StartWorkArgs]) -> list[dict[str, Any]]:
        """Runs `start_work` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("start_work", items)

    def close_snapshot(self) -> None:
        """Runs the app's action."""
        return self.action("close_snapshot")

    def close_snapshot_many(self, items: list[CloseSnapshotArgs]) -> list[dict[str, Any]]:
        """Runs `close_snapshot` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("close_snapshot", items)

    def allow_overwrite(self) -> None:
        """Runs the app's action."""
        return self.action("allow_overwrite")

    def allow_overwrite_many(self, items: list[AllowOverwriteArgs]) -> list[dict[str, Any]]:
        """Runs `allow_overwrite` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("allow_overwrite", items)

    def fold_folder(self,
        *,
        s: Snapshot,
        path: str,
        folded: bool,
    ) -> None:
        """Folds a folder of the snapshot's file index away, or opens it again."""
        return self.action("fold_folder", s=s, path=path, folded=folded)

    def fold_folder_many(self, items: list[FoldFolderArgs]) -> list[dict[str, Any]]:
        """Runs `fold_folder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("fold_folder", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"keep_catalog", "keep_catalog_many", "back_up_now", "back_up_now_many", "verify", "verify_many", "ask_restore", "ask_restore_many", "begin_restore", "begin_restore_many", "restore_now", "restore_now_many", "stop_work", "stop_work_many", "work_moved", "work_moved_many", "work_ended", "work_ended_many", "open_snapshot", "open_snapshot_many", "set_order", "set_order_many", "show_place", "show_place_many", "keep_more", "keep_more_many", "keep_fewer", "keep_fewer_many", "dismiss", "dismiss_many", "choose_source", "choose_source_many", "choose_destination", "choose_destination_many", "go", "go_many", "go_up", "go_up_many", "pick", "pick_many", "cancel_pick", "cancel_pick_many", "read_picker", "read_picker_many", "start_work", "start_work_many", "close_snapshot", "close_snapshot_many", "allow_overwrite", "allow_overwrite_many", "fold_folder", "fold_folder_many"})


class CatalogAsk(Resource):
    """CatalogAsk — `tl.so.tilo.backups.CatalogAsk`."""
    __surface__ = "so.tilo.backups@cf21eed351c07025"
    gbluxeo3: str  # source
    ixnr6cnx: str  # destination


class BackupAsk(Resource):
    """BackupAsk — `tl.so.tilo.backups.BackupAsk`."""
    __surface__ = "so.tilo.backups@cf21eed351c07025"
    w7elqfbk: str  # source
    ebybhxkw: str  # destination
    qqrrtmrs: int  # policy_hourly
    qrbyjmwo: int  # policy_daily
    jd3hqbyz: int  # policy_weekly
    @property
    def _6z7komoa(self) -> int:
        """policy_monthly (`6z7komoa` on the wire)."""
        return self.fields["6z7komoa"]

    @_6z7komoa.setter
    def _6z7komoa(self, value: int) -> None:
        self.edit("6z7komoa", value=value)


class SnapshotAsk(Resource):
    """SnapshotAsk — `tl.so.tilo.backups.SnapshotAsk`."""
    __surface__ = "so.tilo.backups@cf21eed351c07025"
    @property
    def _2stygd3d(self) -> str:
        """destination (`2stygd3d` on the wire)."""
        return self.fields["2stygd3d"]

    @_2stygd3d.setter
    def _2stygd3d(self, value: str) -> None:
        self.edit("2stygd3d", value=value)
    vfb2hw7j: str  # snapshot


class RestoreAsk(Resource):
    """RestoreAsk — `tl.so.tilo.backups.RestoreAsk`."""
    __surface__ = "so.tilo.backups@cf21eed351c07025"
    z2pcuhft: str  # destination
    qz4acm4a: str  # snapshot
    @property
    def _3hxnk3mo(self) -> str:
        """target (`3hxnk3mo` on the wire)."""
        return self.fields["3hxnk3mo"]

    @_3hxnk3mo.setter
    def _3hxnk3mo(self, value: str) -> None:
        self.edit("3hxnk3mo", value=value)
    lbwhdcbx: bool  # overwrite


class WorkDetail(Resource):
    """WorkDetail — `tl.so.tilo.backups.WorkDetail`."""
    __surface__ = "so.tilo.backups@cf21eed351c07025"
    m7xwwoxh: str  # progress
    lwahnmcm: str  # answer


class Checked(Resource):
    """Checked — `tl.so.tilo.backups.Checked`."""
    __surface__ = "so.tilo.backups@cf21eed351c07025"
    lg6ud7lc: int  # files
    deudvrko: int  # bytes


class TiloBackups(Resource):
    """Backups — `tl.so.tilo.backups`."""
    __surface__ = "so.tilo.backups@cf21eed351c07025"

