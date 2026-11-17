"""Types for Diff & Merge (`so.tilo.diffmerge`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Session` is the app's `diffmerge.session` at `diffmerge/<resource>`, read with
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
    "Comparison",
    "HunkChoice",
    "MergeInputs",
    "Session",
    "OpenComparisonArgs",
    "OpenMergeArgs",
    "TakeOursArgs",
    "TakeTheirsArgs",
    "TakeBothArgs",
    "ResolveArgs",
]

class Comparison(TypedDict):
    left_label: str
    left: str
    right_label: str
    right: str

HunkChoice = Literal["ours", "theirs", "both"]

class MergeInputs(TypedDict):
    base_label: str
    base: str
    ours_label: str
    ours: str
    theirs_label: str
    theirs: str

OpenComparisonArgs = TypedDict("OpenComparisonArgs", {
    "left_label": "str",
    "left": "str",
    "right_label": "str",
    "right": "str",
})
"""The arguments of `open-comparison`."""

OpenMergeArgs = TypedDict("OpenMergeArgs", {
    "base_label": "str",
    "base": "str",
    "ours_label": "str",
    "ours": "str",
    "theirs_label": "str",
    "theirs": "str",
})
"""The arguments of `open-merge`."""

TakeOursArgs = TypedDict("TakeOursArgs", {
    "hunk": "int",
})
"""The arguments of `take-ours`."""

TakeTheirsArgs = TypedDict("TakeTheirsArgs", {
    "hunk": "int",
})
"""The arguments of `take-theirs`."""

TakeBothArgs = TypedDict("TakeBothArgs", {
    "hunk": "int",
})
"""The arguments of `take-both`."""

ResolveArgs = TypedDict("ResolveArgs", {
    "hunk": "int",
    "text": "str",
})
"""The arguments of `resolve`."""

class Session(Resource):
    """Diff & Merge — `diffmerge.session`."""
    __surface__ = "so.tilo.diffmerge@6e3e0e40ba1c3ff2"
    @property
    def comparison(self) -> Comparison | None:
        """Read only."""
        return self.fields["comparison"]
    @property
    def merge(self) -> MergeInputs | None:
        """Read only."""
        return self.fields["merge"]
    merged: str
    @property
    def resolutions(self) -> dict[str, HunkChoice]:
        """Read only."""
        return self.fields["resolutions"]
    @property
    def written(self) -> dict[str, str]:
        """Read only."""
        return self.fields["written"]

    def open_comparison(self,
        *,
        left_label: str,
        left: str,
        right_label: str,
        right: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("open-comparison", left_label=left_label, left=left, right_label=right_label, right=right)

    def open_comparison_many(self, items: list[OpenComparisonArgs]) -> list[dict[str, Any]]:
        """Runs `open-comparison` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("open-comparison", items)

    def open_merge(self,
        *,
        base_label: str,
        base: str,
        ours_label: str,
        ours: str,
        theirs_label: str,
        theirs: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("open-merge", base_label=base_label, base=base, ours_label=ours_label, ours=ours, theirs_label=theirs_label, theirs=theirs)

    def open_merge_many(self, items: list[OpenMergeArgs]) -> list[dict[str, Any]]:
        """Runs `open-merge` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("open-merge", items)

    def take_ours(self, *, hunk: int) -> None:
        """Runs the app's action."""
        return self.action("take-ours", hunk=hunk)

    def take_ours_many(self, items: list[TakeOursArgs]) -> list[dict[str, Any]]:
        """Runs `take-ours` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("take-ours", items)

    def take_theirs(self, *, hunk: int) -> None:
        """Runs the app's action."""
        return self.action("take-theirs", hunk=hunk)

    def take_theirs_many(self, items: list[TakeTheirsArgs]) -> list[dict[str, Any]]:
        """Runs `take-theirs` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("take-theirs", items)

    def take_both(self, *, hunk: int) -> None:
        """Runs the app's action."""
        return self.action("take-both", hunk=hunk)

    def take_both_many(self, items: list[TakeBothArgs]) -> list[dict[str, Any]]:
        """Runs `take-both` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("take-both", items)

    def resolve(self,
        *,
        hunk: int,
        text: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("resolve", hunk=hunk, text=text)

    def resolve_many(self, items: list[ResolveArgs]) -> list[dict[str, Any]]:
        """Runs `resolve` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("resolve", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"open_comparison", "open_comparison_many", "open_merge", "open_merge_many", "take_ours", "take_ours_many", "take_theirs", "take_theirs_many", "take_both", "take_both_many", "resolve", "resolve_many"})


