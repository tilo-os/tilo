"""Types for App Store (`so.tilo.store`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Version` is the app's `tl.so.tilo.store.Version` at `store/<resource>`, read with
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
    "Version",
    "Permission",
    "Release",
    "App",
    "Remote",
    "Block",
    "Report",
    "Store",
    "Item",
    "Catalogue",
    "RemoteRead",
    "Details",
    "Review",
    "Case",
    "Prepared",
    "Job",
    "CaseRead",
    "Change",
    "Ask",
    "Started",
    "OneAsk",
    "One",
    "OneRead",
    "RemoteReads",
    "RemoteAsk",
    "BlockAsk",
    "PackageAsk",
    "RuntimeAsk",
    "ReportAsk",
    "SendAsk",
    "StatusAsk",
    "Changes",
    "Items",
    "Evidence",
    "TiloStore",
    "GetArgs",
    "UpdateArgs",
    "InstallRuntimeArgs",
    "PinArgs",
    "UnpinArgs",
    "CancelChangeArgs",
    "SubmitReportArgs",
    "AppealArgs",
    "PollArgs",
    "FollowArgs",
    "BeginReadingArgs",
    "BeginArgs",
    "ReadAppArgs",
    "ShowDetailsArgs",
    "TakeCatalogueArgs",
    "AbsorbRemoteTextArgs",
    "RefreshRemotesArgs",
    "AddRemoteArgs",
    "EnableRemoteArgs",
    "DisableRemoteArgs",
    "RemoteChangeArgs",
    "RemoveRemoteArgs",
    "BrowseArgs",
    "UpdateAllArgs",
    "AddBotArgs",
    "BlockAppArgs",
    "UnblockAppArgs",
    "ImportPackageArgs",
    "InstallReviewedArgs",
    "OpenReferenceArgs",
    "RecordModerationCaseArgs",
    "RefreshStatusArgs",
]

GetArgs = TypedDict("GetArgs", {
})
"""The arguments of `get`."""

UpdateArgs = TypedDict("UpdateArgs", {
})
"""The arguments of `update`."""

InstallRuntimeArgs = TypedDict("InstallRuntimeArgs", {
})
"""The arguments of `install_runtime`."""

PinArgs = TypedDict("PinArgs", {
})
"""The arguments of `pin`."""

UnpinArgs = TypedDict("UnpinArgs", {
})
"""The arguments of `unpin`."""

CancelChangeArgs = TypedDict("CancelChangeArgs", {
})
"""The arguments of `cancel_change`."""

SubmitReportArgs = TypedDict("SubmitReportArgs", {
})
"""The arguments of `submit_report`."""

AppealArgs = TypedDict("AppealArgs", {
})
"""The arguments of `appeal`."""

PollArgs = TypedDict("PollArgs", {
})
"""The arguments of `poll`."""

FollowArgs = TypedDict("FollowArgs", {
    "a": "App",
})
"""The arguments of `follow`."""

BeginReadingArgs = TypedDict("BeginReadingArgs", {
    "op": "str",
    "ask": "str",
})
"""The arguments of `begin_reading`."""

BeginArgs = TypedDict("BeginArgs", {
    "a": "App",
    "op": "str",
    "ask": "str",
})
"""The arguments of `begin`."""

ReadAppArgs = TypedDict("ReadAppArgs", {
    "a": "App",
})
"""The arguments of `read_app`."""

ShowDetailsArgs = TypedDict("ShowDetailsArgs", {
    "a": "App",
})
"""The arguments of `show_details`."""

TakeCatalogueArgs = TypedDict("TakeCatalogueArgs", {
    "status": "str",
})
"""The arguments of `take_catalogue`."""

AbsorbRemoteTextArgs = TypedDict("AbsorbRemoteTextArgs", {
    "text": "str",
})
"""The arguments of `absorb_remote_text`."""

RefreshRemotesArgs = TypedDict("RefreshRemotesArgs", {
})
"""The arguments of `refresh_remotes`."""

AddRemoteArgs = TypedDict("AddRemoteArgs", {
    "name": "str",
    "url": "str",
})
"""The arguments of `add_remote`."""

EnableRemoteArgs = TypedDict("EnableRemoteArgs", {
    "r": "Remote",
})
"""The arguments of `enable_remote`."""

DisableRemoteArgs = TypedDict("DisableRemoteArgs", {
    "r": "Remote",
})
"""The arguments of `disable_remote`."""

RemoteChangeArgs = TypedDict("RemoteChangeArgs", {
    "r": "Remote",
    "enabled": "bool",
})
"""The arguments of `remote_change`."""

RemoveRemoteArgs = TypedDict("RemoveRemoteArgs", {
    "r": "Remote",
})
"""The arguments of `remove_remote`."""

BrowseArgs = TypedDict("BrowseArgs", {
    "r": "Remote",
})
"""The arguments of `browse`."""

UpdateAllArgs = TypedDict("UpdateAllArgs", {
})
"""The arguments of `update_all`."""

AddBotArgs = TypedDict("AddBotArgs", {
    "a": "App",
})
"""The arguments of `add_bot`."""

BlockAppArgs = TypedDict("BlockAppArgs", {
    "a": "App",
})
"""The arguments of `block_app`."""

UnblockAppArgs = TypedDict("UnblockAppArgs", {
    "a": "App",
})
"""The arguments of `unblock_app`."""

ImportPackageArgs = TypedDict("ImportPackageArgs", {
    "file": "Any",
})
"""The arguments of `import_package`."""

InstallReviewedArgs = TypedDict("InstallReviewedArgs", {
})
"""The arguments of `install_reviewed`."""

OpenReferenceArgs = TypedDict("OpenReferenceArgs", {
    "file": "Any",
})
"""The arguments of `open_reference`."""

RecordModerationCaseArgs = TypedDict("RecordModerationCaseArgs", {
    "identity": "str",
    "case": "str",
    "status": "str",
})
"""The arguments of `record_moderation_case`."""

RefreshStatusArgs = TypedDict("RefreshStatusArgs", {
    "r": "Report",
})
"""The arguments of `refresh_status`."""

class Version(Resource):
    """Version — `tl.so.tilo.store.Version`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    tv4yjmd7: str  # version
    @property
    def _2jsas2xh(self) -> str:
        """released (`2jsas2xh` on the wire)."""
        return self.fields["2jsas2xh"]

    @_2jsas2xh.setter
    def _2jsas2xh(self, value: str) -> None:
        self.edit("2jsas2xh", value=value)
    xzuri67c: str  # root
    pryqr455: str  # surface
    j7j23dzx: int  # size
    t73ddwug: str  # runtime
    a6avsz6m: str  # download


class Permission(Resource):
    """Permission — `tl.so.tilo.store.Permission`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    yrcnogu3: str  # what
    znsz4dgp: bool  # broad


class Release(Resource):
    """Release — `tl.so.tilo.store.Release`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    dbblppxc: str  # version
    sil2634m: str  # notes


class App(Resource):
    """App — `tl.so.tilo.store.App`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    bsz5kepr: str  # identity
    z7s7wy7i: str  # name
    @property
    def _7mugzbgk(self) -> str:
        """summary (`7mugzbgk` on the wire)."""
        return self.fields["7mugzbgk"]

    @_7mugzbgk.setter
    def _7mugzbgk(self, value: str) -> None:
        self.edit("7mugzbgk", value=value)
    xmhc7rzm: str  # description
    @property
    def _6igcsadc(self) -> str:
        """developer (`6igcsadc` on the wire)."""
        return self.fields["6igcsadc"]

    @_6igcsadc.setter
    def _6igcsadc(self, value: str) -> None:
        self.edit("6igcsadc", value=value)
    y6tadyrs: str  # license
    s2h3qmpk: list[str]  # categories
    z2ymswmt: str  # icon
    fbg5gcf6: list[str]  # screenshots
    iuswhjxs: str  # homepage
    dmpicgut: str  # source_code
    dyqo2u55: str  # origin
    gkbbivsc: str  # registry
    febktfoe: str  # branch
    qay7cvdi: bool  # user_managed
    hptnn5eg: str  # trust
    kqqhh6as: list[Entry[Version]]  # versions
    sx2epvkk: str  # available
    rssj2htx: str  # installed
    qqimyoka: bool  # pinned
    ev27rmap: str  # fetched
    lmny564e: bool  # runnable
    @property
    def _54f6mqrf(self) -> str:
        """processors (`54f6mqrf` on the wire)."""
        return self.fields["54f6mqrf"]

    @_54f6mqrf.setter
    def _54f6mqrf(self, value: str) -> None:
        self.edit("54f6mqrf", value=value)
    qbk2t2ii: bool  # awaiting_developer
    fmc644bz: int  # featured
    @property
    def _5vq3mjxe(self) -> str:
        """updated (`5vq3mjxe` on the wire)."""
        return self.fields["5vq3mjxe"]

    @_5vq3mjxe.setter
    def _5vq3mjxe(self, value: str) -> None:
        self.edit("5vq3mjxe", value=value)
    aeiz77yf: str  # publisher
    h6fnaygl: str  # digest
    @property
    def _7tdammi6(self) -> bool:
        """bot (`7tdammi6` on the wire)."""
        return self.fields["7tdammi6"]

    @_7tdammi6.setter
    def _7tdammi6(self, value: bool) -> None:
        self.edit("7tdammi6", value=value)
    rnazc2e5: list[str]  # bot_triggers
    su65i54b: list[str]  # bot_apps
    @property
    def _4qzdclse(self) -> list[str]:
        """bot_connectors (`4qzdclse` on the wire)."""
        return self.fields["4qzdclse"]

    @_4qzdclse.setter
    def _4qzdclse(self, value: list[str]) -> None:
        self.edit("4qzdclse", value=value)
    ckhofqel: bool  # bot_appearance
    ehbiqgtb: list[Entry[Permission]]  # permissions
    foea7x37: list[Entry[Release]]  # releases
    fngfv6n4: int  # size
    @property
    def _25c7mbdv(self) -> str:
        """reference (`25c7mbdv` on the wire)."""
        return self.fields["25c7mbdv"]

    @_25c7mbdv.setter
    def _25c7mbdv(self, value: str) -> None:
        self.edit("25c7mbdv", value=value)
    tdkfnlcz: str  # runtime
    qnnzjwd4: str  # runtime_registry
    babs5eju: str  # job
    @property
    def _66hvm5fy(self) -> bool:
        """details_read (`66hvm5fy` on the wire)."""
        return self.fields["66hvm5fy"]

    @_66hvm5fy.setter
    def _66hvm5fy(self, value: bool) -> None:
        self.edit("66hvm5fy", value=value)
    @property
    def _7l5m7ka6(self) -> bool:
        """updatable (`7l5m7ka6` on the wire)."""
        return self.fields["7l5m7ka6"]

    @_7l5m7ka6.setter
    def _7l5m7ka6(self, value: bool) -> None:
        self.edit("7l5m7ka6", value=value)
    awr7kqfn: str  # trouble

    def get(self) -> None:
        """Gets an app the workspace does not run yet: the newest version its source offers."""
        return self.action("get")

    def get_many(self, items: list[GetArgs]) -> list[dict[str, Any]]:
        """Runs `get` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("get", items)

    def update(self) -> None:
        """Updates an app the workspace runs to the newest version its source offers."""
        return self.action("update")

    def update_many(self, items: list[UpdateArgs]) -> list[dict[str, Any]]:
        """Runs `update` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("update", items)

    def install_runtime(self) -> None:
        """Installs the runtime an app needs, once its page reviewed the exact signed offer."""
        return self.action("install_runtime")

    def install_runtime_many(self, items: list[InstallRuntimeArgs]) -> list[dict[str, Any]]:
        """Runs `install_runtime` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("install_runtime", items)

    # the app's `remove` action is the SDK's own: run it with `action("remove")`.
    def pin(self) -> None:
        """Keeps this workspace's version of an app, so it is never cleaned up when unused."""
        return self.action("pin")

    def pin_many(self, items: list[PinArgs]) -> list[dict[str, Any]]:
        """Runs `pin` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("pin", items)

    def unpin(self) -> None:
        """Lets this workspace's unused version of an app be cleaned up again."""
        return self.action("unpin")

    def unpin_many(self, items: list[UnpinArgs]) -> list[dict[str, Any]]:
        """Runs `unpin` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("unpin", items)

    def cancel_change(self) -> None:
        """Stops the change running for an app, keeping what it did until then."""
        return self.action("cancel_change")

    def cancel_change_many(self, items: list[CancelChangeArgs]) -> list[dict[str, Any]]:
        """Runs `cancel_change` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("cancel_change", items)

    def open_app(self) -> None:
        """Opens an app this computer has, fetching it first when the workspace runs it and this computer has not."""
        return self.action("open_app")

    def apply_action(self, *, verb: str) -> None:
        """The action a page's one button runs, by the name the catalogue decided."""
        return self.action("apply_action", verb=verb)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"get", "get_many", "update", "update_many", "install_runtime", "install_runtime_many", "pin", "pin_many", "unpin", "unpin_many", "cancel_change", "cancel_change_many", "open_app", "apply_action"})


class Remote(Resource):
    """Remote — `tl.so.tilo.store.Remote`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    q6znklmy: str  # name
    e74rqhdi: str  # title
    yimcob4p: str  # url
    @property
    def _7o2l4sfb(self) -> str:
        """scope (`7o2l4sfb` on the wire)."""
        return self.fields["7o2l4sfb"]

    @_7o2l4sfb.setter
    def _7o2l4sfb(self, value: str) -> None:
        self.edit("7o2l4sfb", value=value)
    gbitboxx: bool  # enabled
    ixnjbq5u: str  # trouble
    mdalqw5g: int  # count
    pag3getc: bool  # loaded


class Block(Resource):
    """Block — `tl.so.tilo.store.Block`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    qexvvrxm: str  # identity


class Report(Resource):
    """Report — `tl.so.tilo.store.Report`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    i2w5sqnt: str  # identity
    tmhftkva: str  # note
    riwxceey: str  # status
    edrxm3vo: str  # case
    sv5euoyy: str  # operation
    sez3dgh3: str  # registry

    def submit_report(self) -> None:
        """Signs a report with the workspace's identity and sends it to the app's repository."""
        return self.action("submit_report")

    def submit_report_many(self, items: list[SubmitReportArgs]) -> list[dict[str, Any]]:
        """Runs `submit_report` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("submit_report", items)

    def appeal(self) -> None:
        """Appeals a decision about an app."""
        return self.action("appeal")

    def appeal_many(self, items: list[AppealArgs]) -> list[dict[str, Any]]:
        """Runs `appeal` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("appeal", items)

    def report_file(self) -> Any:
        """A report's evidence as the file a person saves and sends with it."""
        return self.action("report_file")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"submit_report", "submit_report_many", "appeal", "appeal_many", "report_file"})


class Store(Resource):
    """Store — `tl.so.tilo.store.Store`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    t6lkc6fh: list[Entry[App]]  # apps
    gvveyw3n: list[Entry[Remote]]  # remotes
    @property
    def _5g4uijlp(self) -> list[Entry[Report]]:
        """reports (`5g4uijlp` on the wire)."""
        return self.fields["5g4uijlp"]

    @_5g4uijlp.setter
    def _5g4uijlp(self, value: list[Entry[Report]]) -> None:
        self.edit("5g4uijlp", value=value)
    a6ibqgbw: list[Entry[Block]]  # blocks
    @property
    def _72gq3jue(self) -> str:
        """status (`72gq3jue` on the wire)."""
        return self.fields["72gq3jue"]

    @_72gq3jue.setter
    def _72gq3jue(self, value: str) -> None:
        self.edit("72gq3jue", value=value)
    sotfrvum: str  # trouble
    eubzl4qx: str  # reading
    e22zgwb6: bool  # flatpak
    uxwwy2fl: str  # flatpak_trouble
    yut35juc: list[str]  # unanswered
    y32wfrks: list[str]  # expired
    @property
    def _452zgqdl(self) -> str:
        """reviewed_root (`452zgqdl` on the wire)."""
        return self.fields["452zgqdl"]

    @_452zgqdl.setter
    def _452zgqdl(self, value: str) -> None:
        self.edit("452zgqdl", value=value)
    @property
    def _5ynamzdl(self) -> str:
        """reviewed_app (`5ynamzdl` on the wire)."""
        return self.fields["5ynamzdl"]

    @_5ynamzdl.setter
    def _5ynamzdl(self, value: str) -> None:
        self.edit("5ynamzdl", value=value)
    vihbdrl7: Any | None  # reviewed_file

    # the app's `refresh` action is the SDK's own: run it with `action("refresh")`.
    def poll(self) -> None:
        """Asks how far the read or the change running has got, and puts what it found into the workspace. Runs while the window is open."""
        return self.action("poll")

    def poll_many(self, items: list[PollArgs]) -> list[dict[str, Any]]:
        """Runs `poll` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("poll", items)

    def follow(self, *, a: App) -> None:
        """The job an app's change is, as far as the engine has got: its row says where it stands, and what it made is read once it ends."""
        return self.action("follow", a=a)

    def follow_many(self, items: list[FollowArgs]) -> list[dict[str, Any]]:
        """Runs `follow` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("follow", items)

    def begin_reading(self,
        *,
        op: str,
        ask: str,
    ) -> None:
        """A job that changes the catalogue itself rather than one app, followed as the read is: its end reads the sources again, so a new repository or an update of everything lands."""
        return self.action("begin_reading", op=op, ask=ask)

    def begin_reading_many(self, items: list[BeginReadingArgs]) -> list[dict[str, Any]]:
        """Runs `begin_reading` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("begin_reading", items)

    def begin(self,
        *,
        a: App,
        op: str,
        ask: str,
    ) -> None:
        """A job the engine started for `a`, or why it did not start."""
        return self.action("begin", a=a, op=op, ask=ask)

    def begin_many(self, items: list[BeginArgs]) -> list[dict[str, Any]]:
        """Runs `begin` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("begin", items)

    def read_app(self, *, a: App) -> None:
        """Reads one app as it stands now: what the workspace runs, what the computer fetched, and the newest offer its source makes."""
        return self.action("read_app", a=a)

    def read_app_many(self, items: list[ReadAppArgs]) -> list[dict[str, Any]]:
        """Runs `read_app` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("read_app", items)

    def show_details(self, *, a: App) -> None:
        """Reads one app's page: what its source says about it."""
        return self.action("show_details", a=a)

    def show_details_many(self, items: list[ShowDetailsArgs]) -> list[dict[str, Any]]:
        """Runs `show_details` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show_details", items)

    def take_catalogue(self, *, status: str) -> None:
        """The catalogue one read found, put into the workspace: apps in the order the sources gave them, each replaced where it stands, and the ones its sources stopped listing taken out."""
        return self.action("take_catalogue", status=status)

    def take_catalogue_many(self, items: list[TakeCatalogueArgs]) -> list[dict[str, Any]]:
        """Runs `take_catalogue` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("take_catalogue", items)

    def absorb_text(self, *, text: str) -> App | None:
        """One app as a source wrote it, read and kept where it stands: an app this workspace already knew keeps what the person looked at of it."""
        return self.action("absorb_text", text=text)

    def absorb_remote_text(self, *, text: str) -> None:
        """One repository as a source wrote it, kept where it stands."""
        return self.action("absorb_remote_text", text=text)

    def absorb_remote_text_many(self, items: list[AbsorbRemoteTextArgs]) -> list[dict[str, Any]]:
        """Runs `absorb_remote_text` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("absorb_remote_text", items)

    def refresh_remotes(self) -> None:
        """Reads this computer's Flatpak repositories again."""
        return self.action("refresh_remotes")

    def refresh_remotes_many(self, items: list[RefreshRemotesArgs]) -> list[dict[str, Any]]:
        """Runs `refresh_remotes` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("refresh_remotes", items)

    def add_remote(self,
        *,
        name: str,
        url: str,
    ) -> None:
        """Adds a Flatpak repository to this workspace."""
        return self.action("add_remote", name=name, url=url)

    def add_remote_many(self, items: list[AddRemoteArgs]) -> list[dict[str, Any]]:
        """Runs `add_remote` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_remote", items)

    def enable_remote(self, *, r: Remote) -> None:
        """Enables one of this workspace's repositories."""
        return self.action("enable_remote", r=r)

    def enable_remote_many(self, items: list[EnableRemoteArgs]) -> list[dict[str, Any]]:
        """Runs `enable_remote` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("enable_remote", items)

    def disable_remote(self, *, r: Remote) -> None:
        """Disables one of this workspace's repositories."""
        return self.action("disable_remote", r=r)

    def disable_remote_many(self, items: list[DisableRemoteArgs]) -> list[dict[str, Any]]:
        """Runs `disable_remote` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("disable_remote", items)

    def remote_change(self,
        *,
        r: Remote,
        enabled: bool,
    ) -> None:
        """Runs the app's action."""
        return self.action("remote_change", r=r, enabled=enabled)

    def remote_change_many(self, items: list[RemoteChangeArgs]) -> list[dict[str, Any]]:
        """Runs `remote_change` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remote_change", items)

    def remove_remote(self, *, r: Remote) -> None:
        """Removes one of this workspace's repositories, and its apps from the catalogue."""
        return self.action("remove_remote", r=r)

    def remove_remote_many(self, items: list[RemoveRemoteArgs]) -> list[dict[str, Any]]:
        """Runs `remove_remote` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_remote", items)

    def browse(self, *, r: Remote) -> None:
        """Browses one repository's apps, through the system Flatpak metadata on this computer."""
        return self.action("browse", r=r)

    def browse_many(self, items: list[BrowseArgs]) -> list[dict[str, Any]]:
        """Runs `browse` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("browse", items)

    def update_all(self) -> None:
        """Updates every app the workspace runs and its sources offer a newer version of."""
        return self.action("update_all")

    def update_all_many(self, items: list[UpdateAllArgs]) -> list[dict[str, Any]]:
        """Runs `update_all` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("update_all", items)

    def add_bot(self, *, a: App) -> None:
        """Adds a directory bot to this workspace as the person's own shortcut: its program is verified against the signed listing and saved as the shortcut the workspace's host runs for them, under the triggers the program declares."""
        return self.action("add_bot", a=a)

    def add_bot_many(self, items: list[AddBotArgs]) -> list[dict[str, Any]]:
        """Runs `add_bot` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_bot", items)

    def block_app(self, *, a: App) -> None:
        """Blocks an app: it then appears nowhere in the Store or in search, and cannot be opened or installed from them. Only the person who blocked it can unblock it."""
        return self.action("block_app", a=a)

    def block_app_many(self, items: list[BlockAppArgs]) -> list[dict[str, Any]]:
        """Runs `block_app` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("block_app", items)

    def unblock_app(self, *, a: App) -> None:
        """Unblocks an app this person blocked. A device's or a workspace's block stays."""
        return self.action("unblock_app", a=a)

    def unblock_app_many(self, items: list[UnblockAppArgs]) -> list[dict[str, Any]]:
        """Runs `unblock_app` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("unblock_app", items)

    def import_package(self, *, file: Any) -> None:
        """Reviews a package file someone hands the Store. Opening one never installs it: its identity must match a verified offer, or an attestor this computer trusts."""
        return self.action("import_package", file=file)

    def import_package_many(self, items: list[ImportPackageArgs]) -> list[dict[str, Any]]:
        """Runs `import_package` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import_package", items)

    def install_reviewed(self) -> None:
        """Installs the package someone just reviewed, after the person read its details."""
        return self.action("install_reviewed")

    def install_reviewed_many(self, items: list[InstallReviewedArgs]) -> list[dict[str, Any]]:
        """Runs `install_reviewed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("install_reviewed", items)

    def open_reference(self, *, file: Any) -> None:
        """Opens a `.flatpakref`: it joins the catalogue as a card, and installing it stays a separate step the person takes."""
        return self.action("open_reference", file=file)

    def open_reference_many(self, items: list[OpenReferenceArgs]) -> list[dict[str, Any]]:
        """Runs `open_reference` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("open_reference", items)

    def review_runtime(self,
        *,
        runtime: str,
        registry: str | None = None,
    ) -> App | None:
        """Reads a runtime's exact signed offer, so its page says what installing it would run."""
        args: dict[str, Any] = {}
        args["runtime"] = runtime
        if registry is not None:
            args["registry"] = registry
        return self.action("review_runtime", **args)

    def draft_report(self, *, a: App) -> Report:
        """Runs the app's action."""
        return self.action("draft_report", a=a)

    def record_moderation_case(self,
        *,
        identity: str,
        case: str,
        status: str,
    ) -> None:
        """Records the registry's case for an app, as a refresh read it."""
        return self.action("record_moderation_case", identity=identity, case=case, status=status)

    def record_moderation_case_many(self, items: list[RecordModerationCaseArgs]) -> list[dict[str, Any]]:
        """Runs `record_moderation_case` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("record_moderation_case", items)

    def prepare_report(self,
        *,
        r: Report,
        appeal: bool,
    ) -> Prepared:
        """Prepares the exact signed operation a report or an appeal sends, without sending it."""
        return self.action("prepare_report", r=r, appeal=appeal)

    def refresh_status(self, *, r: Report) -> None:
        """Reads the registry's verified status for the report's case again. A draft stays a draft until the registry answers."""
        return self.action("refresh_status", r=r)

    def refresh_status_many(self, items: list[RefreshStatusArgs]) -> list[dict[str, Any]]:
        """Runs `refresh_status` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("refresh_status", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"poll", "poll_many", "follow", "follow_many", "begin_reading", "begin_reading_many", "begin", "begin_many", "read_app", "read_app_many", "show_details", "show_details_many", "take_catalogue", "take_catalogue_many", "absorb_text", "absorb_remote_text", "absorb_remote_text_many", "refresh_remotes", "refresh_remotes_many", "add_remote", "add_remote_many", "enable_remote", "enable_remote_many", "disable_remote", "disable_remote_many", "remote_change", "remote_change_many", "remove_remote", "remove_remote_many", "browse", "browse_many", "update_all", "update_all_many", "add_bot", "add_bot_many", "block_app", "block_app_many", "unblock_app", "unblock_app_many", "import_package", "import_package_many", "install_reviewed", "install_reviewed_many", "open_reference", "open_reference_many", "review_runtime", "draft_report", "record_moderation_case", "record_moderation_case_many", "prepare_report", "refresh_status", "refresh_status_many"})


class Item(Resource):
    """Item — `tl.so.tilo.store.Item`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    epxtvzuf: str  # identity
    @property
    def _6a7tsprp(self) -> str:
        """name (`6a7tsprp` on the wire)."""
        return self.fields["6a7tsprp"]

    @_6a7tsprp.setter
    def _6a7tsprp(self, value: str) -> None:
        self.edit("6a7tsprp", value=value)
    ea2bjyc3: str  # summary
    gpwdasim: str  # description
    tns2iluv: str  # developer
    @property
    def _2zltc7rw(self) -> str:
        """license (`2zltc7rw` on the wire)."""
        return self.fields["2zltc7rw"]

    @_2zltc7rw.setter
    def _2zltc7rw(self, value: str) -> None:
        self.edit("2zltc7rw", value=value)
    ukzeb4vu: list[str]  # categories
    uqkifo7y: str  # icon
    @property
    def _5kd5ubmn(self) -> list[str]:
        """screenshots (`5kd5ubmn` on the wire)."""
        return self.fields["5kd5ubmn"]

    @_5kd5ubmn.setter
    def _5kd5ubmn(self, value: list[str]) -> None:
        self.edit("5kd5ubmn", value=value)
    elvkwauw: str  # homepage
    okmosbps: str  # source_code
    mke4chcm: str  # origin
    xbvh2wz3: str  # registry
    cpannwt7: str  # branch
    wikfckou: bool  # user_managed
    nwfophjp: str  # trust
    i4hnlx7u: list[str]  # versions
    ehs57hul: str  # available
    x3qagtfz: str  # installed
    @property
    def _36fsx2jx(self) -> bool:
        """pinned (`36fsx2jx` on the wire)."""
        return self.fields["36fsx2jx"]

    @_36fsx2jx.setter
    def _36fsx2jx(self, value: bool) -> None:
        self.edit("36fsx2jx", value=value)
    cnrxvmkp: str  # fetched
    @property
    def _7jxbeqay(self) -> bool:
        """runnable (`7jxbeqay` on the wire)."""
        return self.fields["7jxbeqay"]

    @_7jxbeqay.setter
    def _7jxbeqay(self, value: bool) -> None:
        self.edit("7jxbeqay", value=value)
    nyj6afge: str  # processors
    jsqjxd7b: bool  # awaiting_developer
    taengdq6: int  # featured
    cvzbzfol: str  # updated
    px6eamz7: str  # publisher
    rga5kuoh: str  # digest
    na6yqlcj: bool  # bot
    lb5drfvf: list[str]  # bot_triggers
    chtwyqcq: list[str]  # bot_apps
    jcezfbrh: list[str]  # bot_connectors
    cj5tnnei: bool  # bot_appearance
    ojgbghet: str  # runtime
    j5umq2va: bool  # updatable
    @property
    def _2nmtgjmg(self) -> str:
        """trouble (`2nmtgjmg` on the wire)."""
        return self.fields["2nmtgjmg"]

    @_2nmtgjmg.setter
    def _2nmtgjmg(self, value: str) -> None:
        self.edit("2nmtgjmg", value=value)


class Catalogue(Resource):
    """Catalogue — `tl.so.tilo.store.Catalogue`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    mr6dwxv6: list[str]  # items
    onje62ck: list[str]  # remotes
    ncdaffvy: str  # status
    kntjub7s: str  # trouble
    av6ojjws: bool  # flatpak
    xe2u37td: str  # flatpak_trouble
    elx5dznp: list[str]  # unanswered
    dhwcxbjv: list[str]  # expired


class RemoteRead(Resource):
    """RemoteRead — `tl.so.tilo.store.RemoteRead`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    rz4t6wig: str  # name
    roitw26x: str  # title
    @property
    def _635v2xjh(self) -> str:
        """url (`635v2xjh` on the wire)."""
        return self.fields["635v2xjh"]

    @_635v2xjh.setter
    def _635v2xjh(self, value: str) -> None:
        self.edit("635v2xjh", value=value)
    xkljozjv: str  # scope
    e4ylyjav: bool  # enabled
    rk5vdazr: str  # trouble


class Details(Resource):
    """Details — `tl.so.tilo.store.Details`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    @property
    def _6cb7jl47(self) -> str:
        """description (`6cb7jl47` on the wire)."""
        return self.fields["6cb7jl47"]

    @_6cb7jl47.setter
    def _6cb7jl47(self, value: str) -> None:
        self.edit("6cb7jl47", value=value)
    @property
    def _7cwn55x3(self) -> str:
        """developer (`7cwn55x3` on the wire)."""
        return self.fields["7cwn55x3"]

    @_7cwn55x3.setter
    def _7cwn55x3(self, value: str) -> None:
        self.edit("7cwn55x3", value=value)
    mptnhu62: str  # license
    a37qv2oa: str  # homepage
    uba5apij: str  # source_code
    nt6mule6: int  # size
    oj36amou: list[str]  # screenshots
    g3nhe33n: list[str]  # permissions
    lygslquo: list[str]  # releases
    whmuitft: str  # runtime
    uijcroeo: str  # runtime_registry
    it2pk5w4: str  # digest


class Review(Resource):
    """Review — `tl.so.tilo.store.Review`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    mzd6fvgn: str  # item
    hxcclid7: str  # root
    oiutpkg7: str  # trouble


class Case(Resource):
    """Case — `tl.so.tilo.store.Case`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    zyv4ydev: str  # case
    hf6ogyp3: str  # status
    o4zyd7r3: str  # situation
    zznn4lla: str  # trouble


class Prepared(Resource):
    """Prepared — `tl.so.tilo.store.Prepared`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    wywxs5oj: str  # operation
    zenjiysp: str  # registry
    owkrdhjj: str  # trouble


class Job(Resource):
    """Job — `tl.so.tilo.store.Job`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    mqizmue3: str  # state
    ygdo3z6c: int  # done
    msrdztlm: int  # total
    fatgfp52: str  # error
    @property
    def _35ydyeno(self) -> str:
        """status (`35ydyeno` on the wire)."""
        return self.fields["35ydyeno"]

    @_35ydyeno.setter
    def _35ydyeno(self, value: str) -> None:
        self.edit("35ydyeno", value=value)


class CaseRead(Resource):
    """CaseRead — `tl.so.tilo.store.CaseRead`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    wi7dxv7v: str  # case
    f67cduxy: str  # status
    @property
    def _5cmcfoao(self) -> str:
        """situation (`5cmcfoao` on the wire)."""
        return self.fields["5cmcfoao"]

    @_5cmcfoao.setter
    def _5cmcfoao(self, value: str) -> None:
        self.edit("5cmcfoao", value=value)


class Change(Resource):
    """Change — `tl.so.tilo.store.Change`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    sr6eixi6: str  # identity
    rkd5rikz: str  # kind
    wfeji5lc: str  # version
    gawnzinv: str  # root
    xz347kth: str  # surface
    wa7a4ebr: str  # publisher
    jalnpzfr: str  # flatpak
    uuhlsrfk: str  # reference
    @property
    def _2usub7jq(self) -> str:
        """file (`2usub7jq` on the wire)."""
        return self.fields["2usub7jq"]

    @_2usub7jq.setter
    def _2usub7jq(self, value: str) -> None:
        self.edit("2usub7jq", value=value)
    smpzmxa2: int  # size


class Ask(Resource):
    """Ask — `tl.so.tilo.store.Ask`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    k33jf5d6: bool  # force


class Started(Resource):
    """Started — `tl.so.tilo.store.Started`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    omjipbxx: str  # job
    haex7z3y: str  # error


class OneAsk(Resource):
    """OneAsk — `tl.so.tilo.store.OneAsk`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    bzdopfzn: str  # identity
    kz4m2q7x: str  # registry


class One(Resource):
    """One — `tl.so.tilo.store.One`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    ibr65ngi: str | None  # item


class OneRead(Resource):
    """OneRead — `tl.so.tilo.store.OneRead`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    gwl2nm5a: Item  # item


class RemoteReads(Resource):
    """RemoteReads — `tl.so.tilo.store.RemoteReads`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    @property
    def _6476ldq6(self) -> list[str]:
        """remotes (`6476ldq6` on the wire)."""
        return self.fields["6476ldq6"]

    @_6476ldq6.setter
    def _6476ldq6(self, value: list[str]) -> None:
        self.edit("6476ldq6", value=value)
    jr4k76wg: bool  # flatpak
    zt4zikpc: str  # trouble


class RemoteAsk(Resource):
    """RemoteAsk — `tl.so.tilo.store.RemoteAsk`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    @property
    def _7ltvsddp(self) -> str:
        """name (`7ltvsddp` on the wire)."""
        return self.fields["7ltvsddp"]

    @_7ltvsddp.setter
    def _7ltvsddp(self, value: str) -> None:
        self.edit("7ltvsddp", value=value)
    p4q2b4s7: str  # url
    vmyl5m7m: bool  # enabled


class BlockAsk(Resource):
    """BlockAsk — `tl.so.tilo.store.BlockAsk`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    dlrecm5k: str  # identity
    xjtxb4gh: bool  # blocked


class PackageAsk(Resource):
    """PackageAsk — `tl.so.tilo.store.PackageAsk`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    fxwybrap: str  # file
    qaju5cxd: str  # name
    bkjyjpcr: int  # size


class RuntimeAsk(Resource):
    """RuntimeAsk — `tl.so.tilo.store.RuntimeAsk`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    vpcuziom: str  # runtime
    snmv7oek: str  # registry


class ReportAsk(Resource):
    """ReportAsk — `tl.so.tilo.store.ReportAsk`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    @property
    def _3x7qwbnh(self) -> str:
        """identity (`3x7qwbnh` on the wire)."""
        return self.fields["3x7qwbnh"]

    @_3x7qwbnh.setter
    def _3x7qwbnh(self, value: str) -> None:
        self.edit("3x7qwbnh", value=value)
    @property
    def _4ibtuh5s(self) -> str:
        """note (`4ibtuh5s` on the wire)."""
        return self.fields["4ibtuh5s"]

    @_4ibtuh5s.setter
    def _4ibtuh5s(self, value: str) -> None:
        self.edit("4ibtuh5s", value=value)
    gea2szrq: str  # case
    @property
    def _6qwyarvg(self) -> str:
        """registry (`6qwyarvg` on the wire)."""
        return self.fields["6qwyarvg"]

    @_6qwyarvg.setter
    def _6qwyarvg(self, value: str) -> None:
        self.edit("6qwyarvg", value=value)
    jjlocy5a: bool  # appeal


class SendAsk(Resource):
    """SendAsk — `tl.so.tilo.store.SendAsk`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    mn7ndrul: str  # identity
    rq53vgig: str  # operation
    f72wtnsy: str  # note


class StatusAsk(Resource):
    """StatusAsk — `tl.so.tilo.store.StatusAsk`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    @property
    def _5xifqzkr(self) -> str:
        """identity (`5xifqzkr` on the wire)."""
        return self.fields["5xifqzkr"]

    @_5xifqzkr.setter
    def _5xifqzkr(self, value: str) -> None:
        self.edit("5xifqzkr", value=value)
    pdu4bxlq: str  # case
    dnrxsz6b: str  # registry


class Changes(Resource):
    """Changes — `tl.so.tilo.store.Changes`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    zxtxkidh: list[Change]  # list


class Items(Resource):
    """Items — `tl.so.tilo.store.Items`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    r4t6n2uh: list[str]  # items


class Evidence(Resource):
    """Evidence — `tl.so.tilo.store.Evidence`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"
    j6n5hmtr: str  # identity
    bnqubb5u: str  # note
    cbhojiz7: str  # status
    t7a6wmgq: str  # case
    ra5kasg4: str  # registry
    dvwnjvtj: str  # operation


class TiloStore(Resource):
    """App Store — `tl.so.tilo.store`."""
    __surface__ = "so.tilo.store@aba934dd0fa05d15"

