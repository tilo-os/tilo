"""Types for Git (`so.tilo.git-client`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Hunk` is the app's `tl.so.tilo.git-client.Hunk` at `git-client/<resource>`, read with
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
    "ChangeArea",
    "ChangeKind",
    "RefKind",
    "DiffLayout",
    "Hunk",
    "Change",
    "RefLabel",
    "Commit",
    "Branch",
    "Tag",
    "Stash",
    "Remote",
    "Worktree",
    "CommitFile",
    "BrowseEntry",
    "Session",
    "Repository",
    "Loaded",
    "WireChange",
    "WireCommit",
    "WireRef",
    "WireBranch",
    "WireTag",
    "WireStash",
    "WireRemote",
    "WireWorktree",
    "WireFile",
    "PathAsk",
    "FileAsk",
    "HunkAsk",
    "CommitAsk",
    "BranchAsk",
    "CheckoutAsk",
    "CommitFilesAsk",
    "WorktreeAsk",
    "StashSaveAsk",
    "StashApplyAsk",
    "ResolveAsk",
    "RemoteAsk",
    "GitClient",
    "ReadFolderArgs",
    "OpenRepositoryArgs",
    "ChooseRepositoryArgs",
    "OpenBrowsedArgs",
    "ClosePickerArgs",
    "StageFileArgs",
    "UnstageFileArgs",
    "DiscardFileArgs",
    "StageAllArgs",
    "UnstageAllArgs",
    "StageHunkArgs",
    "UnstageHunkArgs",
    "DiscardHunkArgs",
    "CommitArgs",
    "ResolveConflictArgs",
    "CreateBranchArgs",
    "CheckoutBranchArgs",
    "SwitchStashingArgs",
    "SwitchCarryingArgs",
    "CancelSwitchArgs",
    "SwitchToArgs",
    "DeleteBranchArgs",
    "SaveStashArgs",
    "ApplyStashArgs",
    "CreateWorktreeArgs",
    "FetchArgs",
    "PullArgs",
    "PushArgs",
    "ReadCommitArgs",
    "ToggleLayoutArgs",
    "ToggleSyntaxArgs",
    "ToggleWhitespaceArgs",
    "ToggleWrapArgs",
    "ClearTroubleArgs",
    "ClearNoticeArgs",
    "AsksArgs",
]

ChangeArea = Literal["staged", "unstaged", "untracked"]
"""Where a change stands: in the next commit, still in the working tree, or not tracked yet."""

ChangeKind = Literal["added", "modified", "deleted", "renamed", "untracked", "conflicted"]
"""What became of a file."""

RefKind = Literal["head", "local_branch", "remote_branch", "tag"]
"""What a ref on a commit is."""

DiffLayout = Literal["unified", "split"]
"""How a diff reads."""

ReadFolderArgs = TypedDict("ReadFolderArgs", {
    "folder": "str",
})
"""The arguments of `read_folder`."""

OpenRepositoryArgs = TypedDict("OpenRepositoryArgs", {
    "path": "str",
})
"""The arguments of `open_repository`."""

ChooseRepositoryArgs = TypedDict("ChooseRepositoryArgs", {
})
"""The arguments of `choose_repository`."""

OpenBrowsedArgs = TypedDict("OpenBrowsedArgs", {
})
"""The arguments of `open_browsed`."""

ClosePickerArgs = TypedDict("ClosePickerArgs", {
})
"""The arguments of `close_picker`."""

StageFileArgs = TypedDict("StageFileArgs", {
    "c": "Change",
})
"""The arguments of `stage_file`."""

UnstageFileArgs = TypedDict("UnstageFileArgs", {
    "c": "Change",
})
"""The arguments of `unstage_file`."""

DiscardFileArgs = TypedDict("DiscardFileArgs", {
    "c": "Change",
})
"""The arguments of `discard_file`."""

StageAllArgs = TypedDict("StageAllArgs", {
})
"""The arguments of `stage_all`."""

UnstageAllArgs = TypedDict("UnstageAllArgs", {
})
"""The arguments of `unstage_all`."""

StageHunkArgs = TypedDict("StageHunkArgs", {
    "c": "Change",
    "hunk": "int",
})
"""The arguments of `stage_hunk`."""

UnstageHunkArgs = TypedDict("UnstageHunkArgs", {
    "c": "Change",
    "hunk": "int",
})
"""The arguments of `unstage_hunk`."""

DiscardHunkArgs = TypedDict("DiscardHunkArgs", {
    "c": "Change",
    "hunk": "int",
})
"""The arguments of `discard_hunk`."""

CommitArgs = TypedDict("CommitArgs", {
    "message": "str",
})
"""The arguments of `commit`."""

ResolveConflictArgs = TypedDict("ResolveConflictArgs", {
    "c": "Change",
    "ours": "bool",
})
"""The arguments of `resolve_conflict`."""

CreateBranchArgs = TypedDict("CreateBranchArgs", {
    "name": "str",
})
"""The arguments of `create_branch`."""

CheckoutBranchArgs = TypedDict("CheckoutBranchArgs", {
    "name": "str",
})
"""The arguments of `checkout_branch`."""

SwitchStashingArgs = TypedDict("SwitchStashingArgs", {
})
"""The arguments of `switch_stashing`."""

SwitchCarryingArgs = TypedDict("SwitchCarryingArgs", {
})
"""The arguments of `switch_carrying`."""

CancelSwitchArgs = TypedDict("CancelSwitchArgs", {
})
"""The arguments of `cancel_switch`."""

SwitchToArgs = TypedDict("SwitchToArgs", {
    "name": "str",
    "stash": "bool",
})
"""The arguments of `switch_to`."""

DeleteBranchArgs = TypedDict("DeleteBranchArgs", {
    "name": "str",
})
"""The arguments of `delete_branch`."""

SaveStashArgs = TypedDict("SaveStashArgs", {
    "message": NotRequired["str | None"],
})
"""The arguments of `save_stash`."""

ApplyStashArgs = TypedDict("ApplyStashArgs", {
    "index": "int",
})
"""The arguments of `apply_stash`."""

CreateWorktreeArgs = TypedDict("CreateWorktreeArgs", {
    "path": "str",
    "branch": "str",
    "start": "str",
})
"""The arguments of `create_worktree`."""

FetchArgs = TypedDict("FetchArgs", {
    "remote": NotRequired["str | None"],
})
"""The arguments of `fetch`."""

PullArgs = TypedDict("PullArgs", {
    "remote": NotRequired["str | None"],
})
"""The arguments of `pull`."""

PushArgs = TypedDict("PushArgs", {
    "remote": NotRequired["str | None"],
})
"""The arguments of `push`."""

ReadCommitArgs = TypedDict("ReadCommitArgs", {
    "id": "str",
})
"""The arguments of `read_commit`."""

ToggleLayoutArgs = TypedDict("ToggleLayoutArgs", {
    "s": "Session",
    "layout": NotRequired["DiffLayout | None"],
})
"""The arguments of `toggle_layout`."""

ToggleSyntaxArgs = TypedDict("ToggleSyntaxArgs", {
    "s": "Session",
    "on": NotRequired["bool | None"],
})
"""The arguments of `toggle_syntax`."""

ToggleWhitespaceArgs = TypedDict("ToggleWhitespaceArgs", {
    "s": "Session",
    "on": NotRequired["bool | None"],
})
"""The arguments of `toggle_whitespace`."""

ToggleWrapArgs = TypedDict("ToggleWrapArgs", {
    "s": "Session",
    "wrap": NotRequired["bool | None"],
})
"""The arguments of `toggle_wrap`."""

ClearTroubleArgs = TypedDict("ClearTroubleArgs", {
})
"""The arguments of `clear_trouble`."""

ClearNoticeArgs = TypedDict("ClearNoticeArgs", {
})
"""The arguments of `clear_notice`."""

AsksArgs = TypedDict("AsksArgs", {
    "s": "Session",
    "op": "str",
    "ask": "str",
})
"""The arguments of `asks`."""

class Hunk(Resource):
    """Hunk — `tl.so.tilo.git-client.Hunk`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    a7uhrt52: int  # index
    @property
    def _5ccadzg6(self) -> str:
        """header (`5ccadzg6` on the wire)."""
        return self.fields["5ccadzg6"]

    @_5ccadzg6.setter
    def _5ccadzg6(self, value: str) -> None:
        self.edit("5ccadzg6", value=value)
    dfethllq: int  # old_start
    pigkwwm6: int  # new_start


class Change(Resource):
    """Change — `tl.so.tilo.git-client.Change`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    ulyz6e5m: str  # path
    pyq3ne6a: str | None  # old_path
    gyf3ucvv: ChangeArea  # area
    a64nnfrl: ChangeKind  # kind
    pgbhcfmd: int  # additions
    cb3hp5ts: int  # deletions
    ddlu6eiw: bool  # binary
    @property
    def _547agrds(self) -> int:
        """ours (`547agrds` on the wire)."""
        return self.fields["547agrds"]

    @_547agrds.setter
    def _547agrds(self, value: int) -> None:
        self.edit("547agrds", value=value)
    gcyh3qk6: int  # theirs
    @property
    def _5bywp6ty(self) -> str:
        """language (`5bywp6ty` on the wire)."""
        return self.fields["5bywp6ty"]

    @_5bywp6ty.setter
    def _5bywp6ty(self, value: str) -> None:
        self.edit("5bywp6ty", value=value)
    @property
    def _2ejdsikb(self) -> str:
        """unified (`2ejdsikb` on the wire)."""
        return self.fields["2ejdsikb"]

    @_2ejdsikb.setter
    def _2ejdsikb(self, value: str) -> None:
        self.edit("2ejdsikb", value=value)
    bdjzphf3: str  # left
    qmisixex: str  # right
    qo6hyuiv: str  # hunks


class RefLabel(Resource):
    """RefLabel — `tl.so.tilo.git-client.RefLabel`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    zl57j56i: str  # name
    @property
    def _7lddqvmy(self) -> RefKind:
        """kind (`7lddqvmy` on the wire)."""
        return self.fields["7lddqvmy"]

    @_7lddqvmy.setter
    def _7lddqvmy(self, value: RefKind) -> None:
        self.edit("7lddqvmy", value=value)


class Commit(Resource):
    """Commit — `tl.so.tilo.git-client.Commit`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    ge4jaage: str  # id
    ktcweris: str  # summary
    qkodo2um: str  # body
    mbs2zev2: str  # author_name
    @property
    def _2rplzpux(self) -> str:
        """author_email (`2rplzpux` on the wire)."""
        return self.fields["2rplzpux"]

    @_2rplzpux.setter
    def _2rplzpux(self, value: str) -> None:
        self.edit("2rplzpux", value=value)
    g5wqmtlu: int  # time
    i5ezibch: int  # age
    ae7mbonn: str  # date
    vngsr7bo: list[str]  # parents
    @property
    def _3k7gvvan(self) -> list[WireRef]:
        """refs (`3k7gvvan` on the wire)."""
        return self.fields["3k7gvvan"]

    @_3k7gvvan.setter
    def _3k7gvvan(self, value: list[WireRef]) -> None:
        self.edit("3k7gvvan", value=value)
    okssmwxo: int  # column


class Branch(Resource):
    """Branch — `tl.so.tilo.git-client.Branch`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    mmht373l: str  # name
    @property
    def _2xd7n6ck(self) -> bool:
        """is_head (`2xd7n6ck` on the wire)."""
        return self.fields["2xd7n6ck"]

    @_2xd7n6ck.setter
    def _2xd7n6ck(self, value: bool) -> None:
        self.edit("2xd7n6ck", value=value)
    ejqxjylh: bool  # is_remote
    vkfjbs4a: str | None  # upstream
    uvswvf53: int  # ahead
    awh54bp4: int  # behind
    fkwngzoz: str  # tip


class Tag(Resource):
    """Tag — `tl.so.tilo.git-client.Tag`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    eimgqjmn: str  # name
    rvt4lxwy: str  # target
    qpus5rj2: str | None  # message


class Stash(Resource):
    """Stash — `tl.so.tilo.git-client.Stash`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    plxxvefl: int  # index
    npo2d33u: str  # message
    kr54kttq: str  # branch


class Remote(Resource):
    """Remote — `tl.so.tilo.git-client.Remote`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    fi4gej3v: str  # name
    ug544zky: str  # url


class Worktree(Resource):
    """Worktree — `tl.so.tilo.git-client.Worktree`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    syhlyvff: str  # name
    s43oholm: str  # path
    zumfqfnq: str  # head
    rcy5osnr: bool  # current


class CommitFile(Resource):
    """CommitFile — `tl.so.tilo.git-client.CommitFile`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    elliscls: str  # path
    tlnyi6uz: str | None  # old_path
    @property
    def _65rizway(self) -> ChangeKind:
        """kind (`65rizway` on the wire)."""
        return self.fields["65rizway"]

    @_65rizway.setter
    def _65rizway(self, value: ChangeKind) -> None:
        self.edit("65rizway", value=value)
    @property
    def _7u7ngqkr(self) -> int:
        """additions (`7u7ngqkr` on the wire)."""
        return self.fields["7u7ngqkr"]

    @_7u7ngqkr.setter
    def _7u7ngqkr(self, value: int) -> None:
        self.edit("7u7ngqkr", value=value)
    qdtrudtf: int  # deletions
    @property
    def _4pwix6ke(self) -> bool:
        """binary (`4pwix6ke` on the wire)."""
        return self.fields["4pwix6ke"]

    @_4pwix6ke.setter
    def _4pwix6ke(self, value: bool) -> None:
        self.edit("4pwix6ke", value=value)
    vgnn6ynm: str  # language
    j2nczx73: str  # unified
    kgmcwjbi: str  # left
    xtgtu5ad: str  # right


class BrowseEntry(Resource):
    """BrowseEntry — `tl.so.tilo.git-client.BrowseEntry`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    mazzivt3: str  # name
    @property
    def _67eqnwql(self) -> str:
        """path (`67eqnwql` on the wire)."""
        return self.fields["67eqnwql"]

    @_67eqnwql.setter
    def _67eqnwql(self, value: str) -> None:
        self.edit("67eqnwql", value=value)
    amedesig: bool  # folder


class Session(Resource):
    """Session — `tl.so.tilo.git-client.Session`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    vrfkfgub: str  # path
    qtrusa64: list[Entry[Change]]  # changes
    qakm3prq: DiffLayout  # layout
    @property
    def _5pb4bgxw(self) -> bool:
        """highlight (`5pb4bgxw` on the wire)."""
        return self.fields["5pb4bgxw"]

    @_5pb4bgxw.setter
    def _5pb4bgxw(self, value: bool) -> None:
        self.edit("5pb4bgxw", value=value)
    m5eoofqc: bool  # whitespace
    mk3evbqh: bool  # wrap
    ma5zpjxi: str  # pending_switch
    taytqzee: bool  # browsing
    cxuhuns5: str  # folder
    ermpuo4w: list[Entry[BrowseEntry]]  # entries
    gpp2fllc: str  # commit_id
    @property
    def _7ihfqmuw(self) -> str:
        """commit_files (`7ihfqmuw` on the wire)."""
        return self.fields["7ihfqmuw"]

    @_7ihfqmuw.setter
    def _7ihfqmuw(self, value: str) -> None:
        self.edit("7ihfqmuw", value=value)
    mr62vl75: str  # notice
    hjgppyqi: str  # trouble

    def read_folder(self, *, folder: str) -> None:
        """Lists `folder` in the repository picker."""
        return self.action("read_folder", folder=folder)

    def read_folder_many(self, items: list[ReadFolderArgs]) -> list[dict[str, Any]]:
        """Runs `read_folder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("read_folder", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"read_folder", "read_folder_many"})


class Repository(Resource):
    """Repository — `tl.so.tilo.git-client.Repository`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    n4wepl5s: str  # path
    @property
    def _6ju4ea2u(self) -> str:
        """name (`6ju4ea2u` on the wire)."""
        return self.fields["6ju4ea2u"]

    @_6ju4ea2u.setter
    def _6ju4ea2u(self, value: str) -> None:
        self.edit("6ju4ea2u", value=value)
    ori4pxhp: str  # head
    nqllokpk: str  # workdir
    ev43omls: str  # author_name
    @property
    def _6ejc2liq(self) -> str:
        """author_email (`6ejc2liq` on the wire)."""
        return self.fields["6ejc2liq"]

    @_6ejc2liq.setter
    def _6ejc2liq(self, value: str) -> None:
        self.edit("6ejc2liq", value=value)
    os3blsxu: str  # snapshot
    vioio4dz: list[Entry[Session]]  # sessions

    # the app's `refresh` action is the SDK's own: run it with `action("refresh")`.
    def open_repository(self, *, path: str) -> None:
        """Opens the repository at `path` on this device."""
        return self.action("open_repository", path=path)

    def open_repository_many(self, items: list[OpenRepositoryArgs]) -> list[dict[str, Any]]:
        """Runs `open_repository` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("open_repository", items)

    def choose_repository(self) -> None:
        """Starts choosing a repository's folder, listing this device's home folder."""
        return self.action("choose_repository")

    def choose_repository_many(self, items: list[ChooseRepositoryArgs]) -> list[dict[str, Any]]:
        """Runs `choose_repository` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("choose_repository", items)

    def open_browsed(self) -> None:
        """Opens the folder the picker shows as a repository."""
        return self.action("open_browsed")

    def open_browsed_many(self, items: list[OpenBrowsedArgs]) -> list[dict[str, Any]]:
        """Runs `open_browsed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("open_browsed", items)

    def close_picker(self) -> None:
        """Puts the picker away."""
        return self.action("close_picker")

    def close_picker_many(self, items: list[ClosePickerArgs]) -> list[dict[str, Any]]:
        """Runs `close_picker` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("close_picker", items)

    def stage_file(self, *, c: Change) -> None:
        """Stages one changed file."""
        return self.action("stage_file", c=c)

    def stage_file_many(self, items: list[StageFileArgs]) -> list[dict[str, Any]]:
        """Runs `stage_file` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("stage_file", items)

    def unstage_file(self, *, c: Change) -> None:
        """Takes one staged file back out of the next commit."""
        return self.action("unstage_file", c=c)

    def unstage_file_many(self, items: list[UnstageFileArgs]) -> list[dict[str, Any]]:
        """Runs `unstage_file` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("unstage_file", items)

    def discard_file(self, *, c: Change) -> None:
        """Discards the working tree's edits to one file, or deletes it when it was never tracked."""
        return self.action("discard_file", c=c)

    def discard_file_many(self, items: list[DiscardFileArgs]) -> list[dict[str, Any]]:
        """Runs `discard_file` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("discard_file", items)

    def stage_all(self) -> None:
        """Stages every change."""
        return self.action("stage_all")

    def stage_all_many(self, items: list[StageAllArgs]) -> list[dict[str, Any]]:
        """Runs `stage_all` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("stage_all", items)

    def unstage_all(self) -> None:
        """Takes every staged change back out of the next commit."""
        return self.action("unstage_all")

    def unstage_all_many(self, items: list[UnstageAllArgs]) -> list[dict[str, Any]]:
        """Runs `unstage_all` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("unstage_all", items)

    def stage_hunk(self,
        *,
        c: Change,
        hunk: int,
    ) -> None:
        """Stages one hunk of a file."""
        return self.action("stage_hunk", c=c, hunk=hunk)

    def stage_hunk_many(self, items: list[StageHunkArgs]) -> list[dict[str, Any]]:
        """Runs `stage_hunk` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("stage_hunk", items)

    def unstage_hunk(self,
        *,
        c: Change,
        hunk: int,
    ) -> None:
        """Takes one staged hunk back out of the next commit."""
        return self.action("unstage_hunk", c=c, hunk=hunk)

    def unstage_hunk_many(self, items: list[UnstageHunkArgs]) -> list[dict[str, Any]]:
        """Runs `unstage_hunk` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("unstage_hunk", items)

    def discard_hunk(self,
        *,
        c: Change,
        hunk: int,
    ) -> None:
        """Discards one hunk of a file's working tree edits."""
        return self.action("discard_hunk", c=c, hunk=hunk)

    def discard_hunk_many(self, items: list[DiscardHunkArgs]) -> list[dict[str, Any]]:
        """Runs `discard_hunk` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("discard_hunk", items)

    def commit(self, *, message: str) -> None:
        """Creates a commit from what is staged."""
        return self.action("commit", message=message)

    def commit_many(self, items: list[CommitArgs]) -> list[dict[str, Any]]:
        """Runs `commit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("commit", items)

    def resolve_conflict(self,
        *,
        c: Change,
        ours: bool,
    ) -> None:
        """Keeps one side of a conflicted file and stages it as resolved."""
        return self.action("resolve_conflict", c=c, ours=ours)

    def resolve_conflict_many(self, items: list[ResolveConflictArgs]) -> list[dict[str, Any]]:
        """Runs `resolve_conflict` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("resolve_conflict", items)

    def create_branch(self, *, name: str) -> None:
        """Creates a branch and switches to it."""
        return self.action("create_branch", name=name)

    def create_branch_many(self, items: list[CreateBranchArgs]) -> list[dict[str, Any]]:
        """Runs `create_branch` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("create_branch", items)

    def checkout_branch(self, *, name: str) -> None:
        """Switches to a branch, asking what becomes of uncommitted work when there is any."""
        return self.action("checkout_branch", name=name)

    def checkout_branch_many(self, items: list[CheckoutBranchArgs]) -> list[dict[str, Any]]:
        """Runs `checkout_branch` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("checkout_branch", items)

    def switch_stashing(self) -> None:
        """Answers the switch question by parking the uncommitted work on the stash first."""
        return self.action("switch_stashing")

    def switch_stashing_many(self, items: list[SwitchStashingArgs]) -> list[dict[str, Any]]:
        """Runs `switch_stashing` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("switch_stashing", items)

    def switch_carrying(self) -> None:
        """Answers the switch question by carrying the uncommitted work along."""
        return self.action("switch_carrying")

    def switch_carrying_many(self, items: list[SwitchCarryingArgs]) -> list[dict[str, Any]]:
        """Runs `switch_carrying` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("switch_carrying", items)

    def cancel_switch(self) -> None:
        """Puts the switch question away."""
        return self.action("cancel_switch")

    def cancel_switch_many(self, items: list[CancelSwitchArgs]) -> list[dict[str, Any]]:
        """Runs `cancel_switch` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("cancel_switch", items)

    def switch_to(self,
        *,
        name: str,
        stash: bool,
    ) -> None:
        """Switches to `name`, parking the uncommitted work when `stash` says so."""
        return self.action("switch_to", name=name, stash=stash)

    def switch_to_many(self, items: list[SwitchToArgs]) -> list[dict[str, Any]]:
        """Runs `switch_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("switch_to", items)

    def delete_branch(self, *, name: str) -> None:
        """Deletes a local branch."""
        return self.action("delete_branch", name=name)

    def delete_branch_many(self, items: list[DeleteBranchArgs]) -> list[dict[str, Any]]:
        """Runs `delete_branch` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_branch", items)

    def save_stash(self,
        *,
        message: str | None = None,
    ) -> None:
        """Stashes the working tree's changes, with a message."""
        args: dict[str, Any] = {}
        if message is not None:
            args["message"] = message
        return self.action("save_stash", **args)

    def save_stash_many(self, items: list[SaveStashArgs]) -> list[dict[str, Any]]:
        """Runs `save_stash` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("save_stash", items)

    def apply_stash(self, *, index: int) -> None:
        """Applies a stash, leaving it in the list."""
        return self.action("apply_stash", index=index)

    def apply_stash_many(self, items: list[ApplyStashArgs]) -> list[dict[str, Any]]:
        """Runs `apply_stash` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("apply_stash", items)

    def create_worktree(self,
        *,
        path: str,
        branch: str,
        start: str,
    ) -> None:
        """Makes a new branch at `start` and a working directory of its own."""
        return self.action("create_worktree", path=path, branch=branch, start=start)

    def create_worktree_many(self, items: list[CreateWorktreeArgs]) -> list[dict[str, Any]]:
        """Runs `create_worktree` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("create_worktree", items)

    def fetch(self,
        *,
        remote: str | None = None,
    ) -> None:
        """Fetches from a remote."""
        args: dict[str, Any] = {}
        if remote is not None:
            args["remote"] = remote
        return self.action("fetch", **args)

    def fetch_many(self, items: list[FetchArgs]) -> list[dict[str, Any]]:
        """Runs `fetch` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("fetch", items)

    def pull(self,
        *,
        remote: str | None = None,
    ) -> None:
        """Fetches, then fast-forwards the current branch to its upstream."""
        args: dict[str, Any] = {}
        if remote is not None:
            args["remote"] = remote
        return self.action("pull", **args)

    def pull_many(self, items: list[PullArgs]) -> list[dict[str, Any]]:
        """Runs `pull` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("pull", items)

    def push(self,
        *,
        remote: str | None = None,
    ) -> None:
        """Pushes the current branch to a like-named branch on a remote."""
        args: dict[str, Any] = {}
        if remote is not None:
            args["remote"] = remote
        return self.action("push", **args)

    def push_many(self, items: list[PushArgs]) -> list[dict[str, Any]]:
        """Runs `push` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("push", items)

    def read_commit(self, *, id: str) -> None:
        """Reads a commit's changed files and their diffs when it is opened."""
        return self.action("read_commit", id=id)

    def read_commit_many(self, items: list[ReadCommitArgs]) -> list[dict[str, Any]]:
        """Runs `read_commit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("read_commit", items)

    def toggle_layout(self,
        *,
        s: Session,
        layout: DiffLayout | None = None,
    ) -> None:
        """Switches how this person reads diffs: one column or two."""
        args: dict[str, Any] = {}
        args["s"] = s
        if layout is not None:
            args["layout"] = layout
        return self.action("toggle_layout", **args)

    def toggle_layout_many(self, items: list[ToggleLayoutArgs]) -> list[dict[str, Any]]:
        """Runs `toggle_layout` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("toggle_layout", items)

    def toggle_syntax(self,
        *,
        s: Session,
        on: bool | None = None,
    ) -> None:
        """Turns syntax colouring on or off for this person."""
        args: dict[str, Any] = {}
        args["s"] = s
        if on is not None:
            args["on"] = on
        return self.action("toggle_syntax", **args)

    def toggle_syntax_many(self, items: list[ToggleSyntaxArgs]) -> list[dict[str, Any]]:
        """Runs `toggle_syntax` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("toggle_syntax", items)

    def toggle_whitespace(self,
        *,
        s: Session,
        on: bool | None = None,
    ) -> None:
        """Shows or hides the whitespace at the end of diff lines for this person."""
        args: dict[str, Any] = {}
        args["s"] = s
        if on is not None:
            args["on"] = on
        return self.action("toggle_whitespace", **args)

    def toggle_whitespace_many(self, items: list[ToggleWhitespaceArgs]) -> list[dict[str, Any]]:
        """Runs `toggle_whitespace` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("toggle_whitespace", items)

    def toggle_wrap(self,
        *,
        s: Session,
        wrap: bool | None = None,
    ) -> None:
        """Wraps long diff lines or lets them run wider than the pane, for this person."""
        args: dict[str, Any] = {}
        args["s"] = s
        if wrap is not None:
            args["wrap"] = wrap
        return self.action("toggle_wrap", **args)

    def toggle_wrap_many(self, items: list[ToggleWrapArgs]) -> list[dict[str, Any]]:
        """Runs `toggle_wrap` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("toggle_wrap", items)

    def clear_trouble(self) -> None:
        """Puts the trouble banner away."""
        return self.action("clear_trouble")

    def clear_trouble_many(self, items: list[ClearTroubleArgs]) -> list[dict[str, Any]]:
        """Runs `clear_trouble` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_trouble", items)

    def clear_notice(self) -> None:
        """Puts the notice banner away."""
        return self.action("clear_notice")

    def clear_notice_many(self, items: list[ClearNoticeArgs]) -> list[dict[str, Any]]:
        """Runs `clear_notice` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_notice", items)

    def asks(self,
        *,
        s: Session,
        op: str,
        ask: str,
    ) -> None:
        """Runs `op` with `ask` and keeps the answer: the repository as it now stands, and the working tree as the device read it."""
        return self.action("asks", s=s, op=op, ask=ask)

    def asks_many(self, items: list[AsksArgs]) -> list[dict[str, Any]]:
        """Runs `asks` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("asks", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"open_repository", "open_repository_many", "choose_repository", "choose_repository_many", "open_browsed", "open_browsed_many", "close_picker", "close_picker_many", "stage_file", "stage_file_many", "unstage_file", "unstage_file_many", "discard_file", "discard_file_many", "stage_all", "stage_all_many", "unstage_all", "unstage_all_many", "stage_hunk", "stage_hunk_many", "unstage_hunk", "unstage_hunk_many", "discard_hunk", "discard_hunk_many", "commit", "commit_many", "resolve_conflict", "resolve_conflict_many", "create_branch", "create_branch_many", "checkout_branch", "checkout_branch_many", "switch_stashing", "switch_stashing_many", "switch_carrying", "switch_carrying_many", "cancel_switch", "cancel_switch_many", "switch_to", "switch_to_many", "delete_branch", "delete_branch_many", "save_stash", "save_stash_many", "apply_stash", "apply_stash_many", "create_worktree", "create_worktree_many", "fetch", "fetch_many", "pull", "pull_many", "push", "push_many", "read_commit", "read_commit_many", "toggle_layout", "toggle_layout_many", "toggle_syntax", "toggle_syntax_many", "toggle_whitespace", "toggle_whitespace_many", "toggle_wrap", "toggle_wrap_many", "clear_trouble", "clear_trouble_many", "clear_notice", "clear_notice_many", "asks", "asks_many"})


class Loaded(Resource):
    """Loaded — `tl.so.tilo.git-client.Loaded`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    u66layc7: str  # answer
    dbwyzz4f: str  # name
    bhabfybv: str  # head
    ifdrbibf: str  # workdir
    eptgnppa: str  # author_name
    zqo4qf2p: str  # author_email
    dijqjokd: list[str]  # changes
    ylfrksiq: list[str]  # commits
    lypvtyhb: list[str]  # branches
    g4pu5gfp: list[str]  # tags
    cvskkv3u: list[str]  # stashes
    sq36vhyn: list[str]  # remotes
    izkg5khe: list[str]  # worktrees
    t3ptayvw: str | None  # worktree_error


class WireChange(Resource):
    """WireChange — `tl.so.tilo.git-client.WireChange`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    @property
    def _37sxq5bs(self) -> str:
        """path (`37sxq5bs` on the wire)."""
        return self.fields["37sxq5bs"]

    @_37sxq5bs.setter
    def _37sxq5bs(self, value: str) -> None:
        self.edit("37sxq5bs", value=value)
    dvptb6yi: str | None  # old_path
    m6m22ctl: ChangeArea  # area
    rck5ywq4: ChangeKind  # kind
    @property
    def _7anyihpj(self) -> int:
        """additions (`7anyihpj` on the wire)."""
        return self.fields["7anyihpj"]

    @_7anyihpj.setter
    def _7anyihpj(self, value: int) -> None:
        self.edit("7anyihpj", value=value)
    kvrcmb3v: int  # deletions
    @property
    def _7dddbyyj(self) -> bool:
        """binary (`7dddbyyj` on the wire)."""
        return self.fields["7dddbyyj"]

    @_7dddbyyj.setter
    def _7dddbyyj(self, value: bool) -> None:
        self.edit("7dddbyyj", value=value)
    zzjs6njo: int  # ours
    ej3kja37: int  # theirs
    tlea3vto: str  # language
    hr5ogyyj: str  # unified
    edfwguh5: str  # left
    @property
    def _2kp7i4ab(self) -> str:
        """right (`2kp7i4ab` on the wire)."""
        return self.fields["2kp7i4ab"]

    @_2kp7i4ab.setter
    def _2kp7i4ab(self, value: str) -> None:
        self.edit("2kp7i4ab", value=value)
    h6xzvamk: list[str]  # hunks


class WireCommit(Resource):
    """WireCommit — `tl.so.tilo.git-client.WireCommit`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    @property
    def _5ycbn6i4(self) -> str:
        """id (`5ycbn6i4` on the wire)."""
        return self.fields["5ycbn6i4"]

    @_5ycbn6i4.setter
    def _5ycbn6i4(self, value: str) -> None:
        self.edit("5ycbn6i4", value=value)
    ugphvrb2: str  # summary
    hg67fbg2: str  # body
    cjxgeuao: str  # author_name
    ypiqlu3b: str  # author_email
    @property
    def _3mge2zfz(self) -> int:
        """time (`3mge2zfz` on the wire)."""
        return self.fields["3mge2zfz"]

    @_3mge2zfz.setter
    def _3mge2zfz(self, value: int) -> None:
        self.edit("3mge2zfz", value=value)
    uyueykhv: int  # age
    o4wtnyc7: str  # date
    o5fnmdlp: list[str]  # parents
    oz6i3qfk: list[str]  # refs
    firuj3kf: int  # column


class WireRef(Resource):
    """WireRef — `tl.so.tilo.git-client.WireRef`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    cgcdv4xh: str  # name
    wlrzlclq: RefKind  # kind


class WireBranch(Resource):
    """WireBranch — `tl.so.tilo.git-client.WireBranch`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    iwhrqigk: str  # name
    lnt6kiuo: bool  # is_head
    i4erkio3: bool  # is_remote
    ej7g53n3: str | None  # upstream
    vupfshru: int  # ahead
    koq4jc7k: int  # behind
    mfbh6kyh: str  # tip


class WireTag(Resource):
    """WireTag — `tl.so.tilo.git-client.WireTag`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    nsgs47is: str  # name
    yn67zzls: str  # target
    uju3pds5: str | None  # message


class WireStash(Resource):
    """WireStash — `tl.so.tilo.git-client.WireStash`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    zyrzfh5t: int  # index
    bd32e2q6: str  # message
    f3lmbvrn: str  # branch


class WireRemote(Resource):
    """WireRemote — `tl.so.tilo.git-client.WireRemote`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    d577aqii: str  # name
    ihlupgbv: str  # url


class WireWorktree(Resource):
    """WireWorktree — `tl.so.tilo.git-client.WireWorktree`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    htbitpma: str  # name
    f3a3kvnf: str  # path
    cqc2kufj: str  # head
    dvbe2xkw: bool  # current


class WireFile(Resource):
    """WireFile — `tl.so.tilo.git-client.WireFile`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    @property
    def _44l2jnvk(self) -> str:
        """path (`44l2jnvk` on the wire)."""
        return self.fields["44l2jnvk"]

    @_44l2jnvk.setter
    def _44l2jnvk(self, value: str) -> None:
        self.edit("44l2jnvk", value=value)
    fr7rebu2: str | None  # old_path
    qggzylu7: ChangeKind  # kind
    b4js3rjp: int  # additions
    ivsbutw6: int  # deletions
    f4hq5otl: bool  # binary
    bjqxfixo: str  # language
    o6jj727v: str  # unified
    oc5lrtao: str  # left
    vmcjzzz5: str  # right


class PathAsk(Resource):
    """PathAsk — `tl.so.tilo.git-client.PathAsk`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    aqyg5aef: str  # path
    dioumhuy: bool  # whitespace


class FileAsk(Resource):
    """FileAsk — `tl.so.tilo.git-client.FileAsk`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    qahiprkz: str  # path
    vywcliib: str  # file


class HunkAsk(Resource):
    """HunkAsk — `tl.so.tilo.git-client.HunkAsk`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    nh7yqayy: str  # path
    vfjaobn2: str  # file
    tprahdo6: int  # hunk
    @property
    def _7pgxgdw3(self) -> bool:
        """unstage (`7pgxgdw3` on the wire)."""
        return self.fields["7pgxgdw3"]

    @_7pgxgdw3.setter
    def _7pgxgdw3(self, value: bool) -> None:
        self.edit("7pgxgdw3", value=value)


class CommitAsk(Resource):
    """CommitAsk — `tl.so.tilo.git-client.CommitAsk`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    ofstus7y: str  # path
    bxihpw7v: str  # message


class BranchAsk(Resource):
    """BranchAsk — `tl.so.tilo.git-client.BranchAsk`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    fi4zi42q: str  # path
    c4fbjyom: str  # name
    l7meu2gh: bool  # checkout


class CheckoutAsk(Resource):
    """CheckoutAsk — `tl.so.tilo.git-client.CheckoutAsk`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    klf3gt64: str  # path
    rpchrkvk: str  # name
    cuml3cz6: bool  # stash


class CommitFilesAsk(Resource):
    """CommitFilesAsk — `tl.so.tilo.git-client.CommitFilesAsk`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    @property
    def _3w5wb2ew(self) -> str:
        """path (`3w5wb2ew` on the wire)."""
        return self.fields["3w5wb2ew"]

    @_3w5wb2ew.setter
    def _3w5wb2ew(self, value: str) -> None:
        self.edit("3w5wb2ew", value=value)
    vbqr55tj: str  # commit


class WorktreeAsk(Resource):
    """WorktreeAsk — `tl.so.tilo.git-client.WorktreeAsk`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    ukwgoqln: str  # path
    py6thmfz: str  # destination
    rwocyieo: str  # branch
    ibmiafbe: str  # start


class StashSaveAsk(Resource):
    """StashSaveAsk — `tl.so.tilo.git-client.StashSaveAsk`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    yeg5b74s: str  # path
    igkwc6uc: str  # message


class StashApplyAsk(Resource):
    """StashApplyAsk — `tl.so.tilo.git-client.StashApplyAsk`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    y3nby6jr: str  # path
    eoe3gfz5: int  # index


class ResolveAsk(Resource):
    """ResolveAsk — `tl.so.tilo.git-client.ResolveAsk`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    yltk3frx: str  # path
    o2dbxhal: str  # file
    qvivrkzl: bool  # ours


class RemoteAsk(Resource):
    """RemoteAsk — `tl.so.tilo.git-client.RemoteAsk`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"
    gifph57t: str  # path
    tz3p3qeb: str  # remote


class GitClient(Resource):
    """Git — `tl.so.tilo.git-client`."""
    __surface__ = "so.tilo.git-client@b0e5ccf8496b432f"

