"""Types for Diff & Merge (`so.tilo.diffmerge`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Block` is the app's `tl.so.tilo.diffmerge.Block` at `diffmerge/<resource>`, read with
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
    "Block",
    "Hunk",
    "FolderEntry",
    "Session",
    "Library",
    "OpenAsk",
    "OpenedAnswer",
    "DiffAsk",
    "DiffAnswer",
    "MergeAsk",
    "MergeAnswer",
    "ComposeAsk",
    "ComposedAnswer",
    "FoldersAsk",
    "FoldersAnswer",
    "SaveAsk",
    "SavedAnswer",
    "SampleAsk",
    "SampleDiff",
    "SampleMerge",
    "Diffmerge",
    "TakeOursArgs",
    "TakeTheirsArgs",
    "TakeBothArgs",
    "ResolveArgs",
    "CompareFoldersArgs",
    "ChosenFolderLeftArgs",
    "ChosenFolderRightArgs",
    "EditMergedArgs",
    "SaveMergedArgs",
    "FillSampleArgs",
    "SampleFoldersArgs",
    "ClearNoticeArgs",
    "ForgetArgs",
]

TakeOursArgs = TypedDict("TakeOursArgs", {
    "hunk": "int",
})
"""The arguments of `take_ours`."""

TakeTheirsArgs = TypedDict("TakeTheirsArgs", {
    "hunk": "int",
})
"""The arguments of `take_theirs`."""

TakeBothArgs = TypedDict("TakeBothArgs", {
    "hunk": "int",
})
"""The arguments of `take_both`."""

ResolveArgs = TypedDict("ResolveArgs", {
    "hunk": "int",
    "text": "str",
})
"""The arguments of `resolve`."""

CompareFoldersArgs = TypedDict("CompareFoldersArgs", {
    "left": NotRequired["str | None"],
    "right": NotRequired["str | None"],
})
"""The arguments of `compare_folders`."""

ChosenFolderLeftArgs = TypedDict("ChosenFolderLeftArgs", {
    "folder": "str",
})
"""The arguments of `chosen_folder_left`."""

ChosenFolderRightArgs = TypedDict("ChosenFolderRightArgs", {
    "folder": "str",
})
"""The arguments of `chosen_folder_right`."""

EditMergedArgs = TypedDict("EditMergedArgs", {
    "text": "str",
})
"""The arguments of `edit_merged`."""

SaveMergedArgs = TypedDict("SaveMergedArgs", {
    "path": "str",
})
"""The arguments of `save_merged`."""

FillSampleArgs = TypedDict("FillSampleArgs", {
})
"""The arguments of `fill_sample`."""

SampleFoldersArgs = TypedDict("SampleFoldersArgs", {
})
"""The arguments of `sample_folders`."""

ClearNoticeArgs = TypedDict("ClearNoticeArgs", {
})
"""The arguments of `clear_notice`."""

ForgetArgs = TypedDict("ForgetArgs", {
    "s": "Session",
})
"""The arguments of `forget`."""

class Block(Resource):
    """Block — `tl.so.tilo.diffmerge.Block`."""
    __surface__ = "so.tilo.diffmerge@4bbe25b0f9538b02"
    csjrkfwp: int  # first
    cjhoemux: int  # last
    w2shjabm: str  # kind
    vwniktmt: str  # preview


class Hunk(Resource):
    """Hunk — `tl.so.tilo.diffmerge.Hunk`."""
    __surface__ = "so.tilo.diffmerge@4bbe25b0f9538b02"
    @property
    def _47hfb7ea(self) -> int:
        """index (`47hfb7ea` on the wire)."""
        return self.fields["47hfb7ea"]

    @_47hfb7ea.setter
    def _47hfb7ea(self, value: int) -> None:
        self.edit("47hfb7ea", value=value)
    @property
    def _4qm5ujaf(self) -> str:
        """resolution (`4qm5ujaf` on the wire)."""
        return self.fields["4qm5ujaf"]

    @_4qm5ujaf.setter
    def _4qm5ujaf(self, value: str) -> None:
        self.edit("4qm5ujaf", value=value)
    m2r6lioa: str  # base
    msvfenbm: str  # ours
    ojqeurrr: str  # theirs


class FolderEntry(Resource):
    """FolderEntry — `tl.so.tilo.diffmerge.FolderEntry`."""
    __surface__ = "so.tilo.diffmerge@4bbe25b0f9538b02"
    usttvqtv: str  # path
    q6kpasi4: str  # kind
    u5jul62r: int  # left_len
    @property
    def _4in5gnvl(self) -> int:
        """right_len (`4in5gnvl` on the wire)."""
        return self.fields["4in5gnvl"]

    @_4in5gnvl.setter
    def _4in5gnvl(self, value: int) -> None:
        self.edit("4in5gnvl", value=value)
    dcnmuoj3: bool  # by_length


class Session(Resource):
    """Session — `tl.so.tilo.diffmerge.Session`."""
    __surface__ = "so.tilo.diffmerge@4bbe25b0f9538b02"
    j6uarraa: str  # name
    odsfatc7: str  # left_label
    kcdcrgzi: str  # left
    @property
    def _3q7df7eg(self) -> str:
        """right_label (`3q7df7eg` on the wire)."""
        return self.fields["3q7df7eg"]

    @_3q7df7eg.setter
    def _3q7df7eg(self, value: str) -> None:
        self.edit("3q7df7eg", value=value)
    prwrxrg4: str  # right
    ebpdel6u: str  # left_aligned
    vjlsurzf: str  # right_aligned
    @property
    def _7obphfng(self) -> str:
        """kinds (`7obphfng` on the wire)."""
        return self.fields["7obphfng"]

    @_7obphfng.setter
    def _7obphfng(self, value: str) -> None:
        self.edit("7obphfng", value=value)
    ogpmdy55: list[Entry[Block]]  # blocks
    ie5v4a4i: int  # added
    hv4ba7bq: int  # removed
    @property
    def _6a2srqu5(self) -> int:
        """modified (`6a2srqu5` on the wire)."""
        return self.fields["6a2srqu5"]

    @_6a2srqu5.setter
    def _6a2srqu5(self, value: int) -> None:
        self.edit("6a2srqu5", value=value)
    @property
    def _2f7twk55(self) -> bool:
        """identical (`2f7twk55` on the wire)."""
        return self.fields["2f7twk55"]

    @_2f7twk55.setter
    def _2f7twk55(self, value: bool) -> None:
        self.edit("2f7twk55", value=value)
    @property
    def _34arfoar(self) -> bool:
        """compared (`34arfoar` on the wire)."""
        return self.fields["34arfoar"]

    @_34arfoar.setter
    def _34arfoar(self, value: bool) -> None:
        self.edit("34arfoar", value=value)
    mhtp452w: str  # base_label
    @property
    def _5ozczfsi(self) -> str:
        """base (`5ozczfsi` on the wire)."""
        return self.fields["5ozczfsi"]

    @_5ozczfsi.setter
    def _5ozczfsi(self, value: str) -> None:
        self.edit("5ozczfsi", value=value)
    czecclw6: str  # ours_label
    @property
    def _75vbkvt3(self) -> str:
        """ours (`75vbkvt3` on the wire)."""
        return self.fields["75vbkvt3"]

    @_75vbkvt3.setter
    def _75vbkvt3(self, value: str) -> None:
        self.edit("75vbkvt3", value=value)
    @property
    def _6iwqekxm(self) -> str:
        """theirs_label (`6iwqekxm` on the wire)."""
        return self.fields["6iwqekxm"]

    @_6iwqekxm.setter
    def _6iwqekxm(self, value: str) -> None:
        self.edit("6iwqekxm", value=value)
    zm3cqlts: str  # theirs
    c7es3yev: str  # merged
    twukcquq: str  # composed
    gvqybypj: list[Entry[Hunk]]  # hunks
    bolkamra: int  # changes
    mjnnfnbm: int  # conflicts
    n7puwfba: bool  # merging
    yif52d5b: str  # folder_choice_left
    cr3zcxiz: str  # folder_choice_right
    w5y6trs7: str  # folder_left_label
    @property
    def _7i3hhhy2(self) -> str:
        """folder_right_label (`7i3hhhy2` on the wire)."""
        return self.fields["7i3hhhy2"]

    @_7i3hhhy2.setter
    def _7i3hhhy2(self, value: str) -> None:
        self.edit("7i3hhhy2", value=value)
    mbawdpwr: str  # folder_left_root
    utsx5nqp: str  # folder_right_root
    tn6u7t4i: list[Entry[FolderEntry]]  # folder_entries
    @property
    def _7ceymc4l(self) -> bool:
        """folder_truncated (`7ceymc4l` on the wire)."""
        return self.fields["7ceymc4l"]

    @_7ceymc4l.setter
    def _7ceymc4l(self, value: bool) -> None:
        self.edit("7ceymc4l", value=value)
    z4cg3ljy: bool  # folding
    toc5vveo: str  # saved_path
    avjlapbd: str  # saved_name
    nkku72cz: str  # saved
    ttqrgoca: str  # notice
    dr64yk6f: str  # notice_kind

    def open_comparison(self,
        *,
        left_label: str,
        left: str,
        right_label: str,
        right: str,
    ) -> Session:
        """Compares two texts: what a script, an agent or another app asks of Diff & Merge."""
        return self.action("open_comparison", left_label=left_label, left=left, right_label=right_label, right=right)

    def chosen_left(self, *, file: Any) -> Session:
        """The action the person's file chooser fills for the left side: the left file, and then the right one."""
        return self.action("chosen_left", file=file)

    def chosen_right(self, *, file: Any) -> Session:
        """The action the person's file chooser fills for the right side."""
        return self.action("chosen_right", file=file)

    def refresh_comparison(self) -> bool:
        """Asks the device to align the two sides, and holds what it made: the aligned columns, the rows that changed and how many of each."""
        return self.action("refresh_comparison")

    def open_merge(self,
        *,
        base_label: str,
        base: str,
        ours_label: str,
        ours: str,
        theirs_label: str,
        theirs: str,
    ) -> Session:
        """Merges three texts: a common base and the two versions of it a person has."""
        return self.action("open_merge", base_label=base_label, base=base, ours_label=ours_label, ours=ours, theirs_label=theirs_label, theirs=theirs)

    def refresh_merge(self) -> bool:
        """Asks the device to hunk the merge and compose the result with what the person took so far."""
        return self.action("refresh_merge")

    def take_ours(self, *, hunk: int) -> None:
        """Takes our side for a change."""
        return self.action("take_ours", hunk=hunk)

    def take_ours_many(self, items: list[TakeOursArgs]) -> list[dict[str, Any]]:
        """Runs `take_ours` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("take_ours", items)

    def take_theirs(self, *, hunk: int) -> None:
        """Takes their side for a change."""
        return self.action("take_theirs", hunk=hunk)

    def take_theirs_many(self, items: list[TakeTheirsArgs]) -> list[dict[str, Any]]:
        """Runs `take_theirs` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("take_theirs", items)

    def take_both(self, *, hunk: int) -> None:
        """Takes both sides for a change, ours first."""
        return self.action("take_both", hunk=hunk)

    def take_both_many(self, items: list[TakeBothArgs]) -> list[dict[str, Any]]:
        """Runs `take_both` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("take_both", items)

    def resolve(self,
        *,
        hunk: int,
        text: str,
    ) -> None:
        """Resolves a change with the person's own text."""
        return self.action("resolve", hunk=hunk, text=text)

    def resolve_many(self, items: list[ResolveArgs]) -> list[dict[str, Any]]:
        """Runs `resolve` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("resolve", items)

    def take(self,
        *,
        hunk: int,
        resolution: str,
    ) -> bool:
        """Runs the app's action."""
        return self.action("take", hunk=hunk, resolution=resolution)

    def compose_into(self) -> bool:
        """Runs the app's action."""
        return self.action("compose_into")

    def compare_folders(self,
        *,
        left: str | None = None,
        right: str | None = None,
    ) -> None:
        """Compares the two folders at `left` and `right` on this device: what only one holds, and what both hold but differ in. Two absolute paths, or two empty ones to choose the folders at the window."""
        args: dict[str, Any] = {}
        if left is not None:
            args["left"] = left
        if right is not None:
            args["right"] = right
        return self.action("compare_folders", **args)

    def compare_folders_many(self, items: list[CompareFoldersArgs]) -> list[dict[str, Any]]:
        """Runs `compare_folders` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("compare_folders", items)

    def hold_folders(self, *, answer: str) -> bool:
        """Runs the app's action."""
        return self.action("hold_folders", answer=answer)

    def chosen_folder_left(self, *, folder: str) -> None:
        """The action the person's folder chooser fills for the left tree."""
        return self.action("chosen_folder_left", folder=folder)

    def chosen_folder_left_many(self, items: list[ChosenFolderLeftArgs]) -> list[dict[str, Any]]:
        """Runs `chosen_folder_left` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("chosen_folder_left", items)

    def chosen_folder_right(self, *, folder: str) -> None:
        """The action the person's folder chooser fills for the right tree: both chosen, the folders are compared."""
        return self.action("chosen_folder_right", folder=folder)

    def chosen_folder_right_many(self, items: list[ChosenFolderRightArgs]) -> list[dict[str, Any]]:
        """Runs `chosen_folder_right` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("chosen_folder_right", items)

    def open_folder_entry(self, *, entry: FolderEntry) -> Session:
        """Opens one file of the folder comparison: both its sides when both trees hold it, else the one that does, against nothing."""
        return self.action("open_folder_entry", entry=entry)

    def open_on_key(self,
        *,
        entry: FolderEntry,
        key: str,
    ) -> Session:
        """Opens the file of the row the person pressed, for the key that asked: Enter opens it."""
        return self.action("open_on_key", entry=entry, key=key)

    def edit_merged(self, *, text: str) -> None:
        """Replaces the merged result: what a person types into it, and what an agent or a script writes when the result is theirs to change. Once it is not what the sides compose, taking a side would throw the edit away, so the rest is resolved in the text."""
        return self.action("edit_merged", text=text)

    def edit_merged_many(self, items: list[EditMergedArgs]) -> list[dict[str, Any]]:
        """Runs `edit_merged` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("edit_merged", items)

    def save_merged(self, *, path: str) -> None:
        """Writes the merged result to `path` on this device."""
        return self.action("save_merged", path=path)

    def save_merged_many(self, items: list[SaveMergedArgs]) -> list[dict[str, Any]]:
        """Runs `save_merged` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("save_merged", items)

    def save_bytes(self) -> Any:
        """The merged result as a file the person places with the system's save chooser, which the window shows."""
        return self.action("save_bytes")

    def fill_sample(self) -> None:
        """Fills a comparison with the samples: the two-way comparison and the three-way merge, so a person sees how each reads before they have two files at hand."""
        return self.action("fill_sample")

    def fill_sample_many(self, items: list[FillSampleArgs]) -> list[dict[str, Any]]:
        """Runs `fill_sample` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("fill_sample", items)

    def sample_folders(self) -> None:
        """Shows the sample folder comparison: no disk is read for it."""
        return self.action("sample_folders")

    def sample_folders_many(self, items: list[SampleFoldersArgs]) -> list[dict[str, Any]]:
        """Runs `sample_folders` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("sample_folders", items)

    def refuse(self, *, why: str) -> bool:
        """Runs the app's action."""
        return self.action("refuse", why=why)

    def clear_notice(self) -> None:
        """Runs the app's action."""
        return self.action("clear_notice")

    def clear_notice_many(self, items: list[ClearNoticeArgs]) -> list[dict[str, Any]]:
        """Runs `clear_notice` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_notice", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"open_comparison", "chosen_left", "chosen_right", "refresh_comparison", "open_merge", "refresh_merge", "take_ours", "take_ours_many", "take_theirs", "take_theirs_many", "take_both", "take_both_many", "resolve", "resolve_many", "take", "compose_into", "compare_folders", "compare_folders_many", "hold_folders", "chosen_folder_left", "chosen_folder_left_many", "chosen_folder_right", "chosen_folder_right_many", "open_folder_entry", "open_on_key", "edit_merged", "edit_merged_many", "save_merged", "save_merged_many", "save_bytes", "fill_sample", "fill_sample_many", "sample_folders", "sample_folders_many", "refuse", "clear_notice", "clear_notice_many"})


class Library(Resource):
    """Library — `tl.so.tilo.diffmerge.Library`."""
    __surface__ = "so.tilo.diffmerge@4bbe25b0f9538b02"
    tzmh25hr: list[Entry[Session]]  # sessions

    def new_comparison(self) -> Session:
        """Starts an empty comparison: a place to open two files in."""
        return self.action("new_comparison")

    def open_files(self,
        *,
        left: str | None = None,
        right: str | None = None,
    ) -> Session:
        """Opens the two files at `left` and `right` on this device and compares them. Two absolute paths, or two empty ones to choose the files at the window."""
        args: dict[str, Any] = {}
        if left is not None:
            args["left"] = left
        if right is not None:
            args["right"] = right
        return self.action("open_files", **args)

    def open_one(self,
        *,
        path: str,
        side: str | None = None,
    ) -> Session:
        """Opens one file on this device against nothing: what a folder comparison shows for a file only one tree holds. `side` is `left` or `right`."""
        args: dict[str, Any] = {}
        args["path"] = path
        if side is not None:
            args["side"] = side
        return self.action("open_one", **args)

    def load_sample(self) -> Session:
        """Opens the sample comparison and the sample merge, so a person sees how each reads before they have two files at hand."""
        return self.action("load_sample")

    def forget(self, *, s: Session) -> None:
        """Runs the app's action."""
        return self.action("forget", s=s)

    def forget_many(self, items: list[ForgetArgs]) -> list[dict[str, Any]]:
        """Runs `forget` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("forget", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_comparison", "open_files", "open_one", "load_sample", "forget", "forget_many"})


class OpenAsk(Resource):
    """OpenAsk — `tl.so.tilo.diffmerge.OpenAsk`."""
    __surface__ = "so.tilo.diffmerge@4bbe25b0f9538b02"
    ym5ajbxk: str  # path
    ou5hf2bx: str  # address
    qqwavfqy: str  # name


class OpenedAnswer(Resource):
    """OpenedAnswer — `tl.so.tilo.diffmerge.OpenedAnswer`."""
    __surface__ = "so.tilo.diffmerge@4bbe25b0f9538b02"
    f4rrknsp: str  # label
    ced6vdp4: str  # text


class DiffAsk(Resource):
    """DiffAsk — `tl.so.tilo.diffmerge.DiffAsk`."""
    __surface__ = "so.tilo.diffmerge@4bbe25b0f9538b02"
    vz6ogvrp: str  # left
    @property
    def _3upzelrr(self) -> str:
        """right (`3upzelrr` on the wire)."""
        return self.fields["3upzelrr"]

    @_3upzelrr.setter
    def _3upzelrr(self, value: str) -> None:
        self.edit("3upzelrr", value=value)


class DiffAnswer(Resource):
    """DiffAnswer — `tl.so.tilo.diffmerge.DiffAnswer`."""
    __surface__ = "so.tilo.diffmerge@4bbe25b0f9538b02"
    hahxq35a: list[int]  # block_firsts
    jx4fqkfc: list[int]  # block_lasts
    we3fy74x: list[str]  # block_kinds
    xmvhnvex: list[str]  # block_previews
    fejavrie: str  # left
    zmrjesu2: str  # right
    psvy4e5w: str  # rows
    akgopqc4: int  # added
    @property
    def _2wokc6qk(self) -> int:
        """removed (`2wokc6qk` on the wire)."""
        return self.fields["2wokc6qk"]

    @_2wokc6qk.setter
    def _2wokc6qk(self, value: int) -> None:
        self.edit("2wokc6qk", value=value)
    w6mvjrvf: int  # modified
    frxu6oxq: bool  # identical


class MergeAsk(Resource):
    """MergeAsk — `tl.so.tilo.diffmerge.MergeAsk`."""
    __surface__ = "so.tilo.diffmerge@4bbe25b0f9538b02"
    vx52jl2g: str  # base
    mrxlgkxk: str  # ours
    nvyd6cgn: str  # theirs


class MergeAnswer(Resource):
    """MergeAnswer — `tl.so.tilo.diffmerge.MergeAnswer`."""
    __surface__ = "so.tilo.diffmerge@4bbe25b0f9538b02"
    @property
    def _2pwajyjm(self) -> list[int]:
        """hunk_indices (`2pwajyjm` on the wire)."""
        return self.fields["2pwajyjm"]

    @_2pwajyjm.setter
    def _2pwajyjm(self, value: list[int]) -> None:
        self.edit("2pwajyjm", value=value)
    nyvwoswu: list[str]  # hunk_resolutions
    imh4txwm: list[str]  # hunk_bases
    fra5wt4r: list[str]  # hunk_ours
    y56numrx: list[str]  # hunk_theirs
    h6l3j5y3: str  # merged
    e7auw454: int  # changes
    oqot5haf: int  # conflicts


class ComposeAsk(Resource):
    """ComposeAsk — `tl.so.tilo.diffmerge.ComposeAsk`."""
    __surface__ = "so.tilo.diffmerge@4bbe25b0f9538b02"
    @property
    def _55fy4avh(self) -> str:
        """base (`55fy4avh` on the wire)."""
        return self.fields["55fy4avh"]

    @_55fy4avh.setter
    def _55fy4avh(self, value: str) -> None:
        self.edit("55fy4avh", value=value)
    fxqifsds: str  # ours
    @property
    def _6te7azkd(self) -> str:
        """theirs (`6te7azkd` on the wire)."""
        return self.fields["6te7azkd"]

    @_6te7azkd.setter
    def _6te7azkd(self, value: str) -> None:
        self.edit("6te7azkd", value=value)
    atnd6awc: list[str]  # choices
    weznhatv: list[str]  # written


class ComposedAnswer(Resource):
    """ComposedAnswer — `tl.so.tilo.diffmerge.ComposedAnswer`."""
    __surface__ = "so.tilo.diffmerge@4bbe25b0f9538b02"
    wino2rjt: str  # merged
    niqoebpg: int  # changes
    gzqibeka: int  # conflicts


class FoldersAsk(Resource):
    """FoldersAsk — `tl.so.tilo.diffmerge.FoldersAsk`."""
    __surface__ = "so.tilo.diffmerge@4bbe25b0f9538b02"
    jasvo2t2: str  # left
    @property
    def _2go4z2dd(self) -> str:
        """right (`2go4z2dd` on the wire)."""
        return self.fields["2go4z2dd"]

    @_2go4z2dd.setter
    def _2go4z2dd(self, value: str) -> None:
        self.edit("2go4z2dd", value=value)


class FoldersAnswer(Resource):
    """FoldersAnswer — `tl.so.tilo.diffmerge.FoldersAnswer`."""
    __surface__ = "so.tilo.diffmerge@4bbe25b0f9538b02"
    uytgg4wc: str  # left_label
    bxame25h: str  # left_root
    ewdcrys5: str  # right_label
    ogpczjbg: str  # right_root
    wb25c6a6: list[str]  # entry_paths
    n2bmepxt: list[str]  # entry_kinds
    bbb24qp5: list[int]  # entry_left_lens
    mldndym5: list[int]  # entry_right_lens
    @property
    def _7nr427ip(self) -> list[bool]:
        """entry_by_lengths (`7nr427ip` on the wire)."""
        return self.fields["7nr427ip"]

    @_7nr427ip.setter
    def _7nr427ip(self, value: list[bool]) -> None:
        self.edit("7nr427ip", value=value)
    hqjkq6oq: bool  # truncated


class SaveAsk(Resource):
    """SaveAsk — `tl.so.tilo.diffmerge.SaveAsk`."""
    __surface__ = "so.tilo.diffmerge@4bbe25b0f9538b02"
    t2ifxuue: str  # path
    zoq67abl: str  # text


class SavedAnswer(Resource):
    """SavedAnswer — `tl.so.tilo.diffmerge.SavedAnswer`."""
    __surface__ = "so.tilo.diffmerge@4bbe25b0f9538b02"
    tmjabrvg: str  # name


class SampleAsk(Resource):
    """SampleAsk — `tl.so.tilo.diffmerge.SampleAsk`."""
    __surface__ = "so.tilo.diffmerge@4bbe25b0f9538b02"
    ol4drxwd: str  # kind


class SampleDiff(Resource):
    """SampleDiff — `tl.so.tilo.diffmerge.SampleDiff`."""
    __surface__ = "so.tilo.diffmerge@4bbe25b0f9538b02"
    uj6xvzvl: str  # left_label
    cjwd2i2z: str  # left
    vbhvxj6f: str  # right_label
    whunzkq3: str  # right


class SampleMerge(Resource):
    """SampleMerge — `tl.so.tilo.diffmerge.SampleMerge`."""
    __surface__ = "so.tilo.diffmerge@4bbe25b0f9538b02"
    opwxdlzs: str  # base_label
    l3kqbcv7: str  # base
    wxfngzy5: str  # ours_label
    ubqyr5rh: str  # ours
    amhqg7cn: str  # theirs_label
    wfaxbaim: str  # theirs


class Diffmerge(Resource):
    """Diff & Merge — `tl.so.tilo.diffmerge`."""
    __surface__ = "so.tilo.diffmerge@4bbe25b0f9538b02"

