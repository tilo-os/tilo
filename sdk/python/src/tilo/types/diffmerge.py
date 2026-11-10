"""Types for Diff & Merge (`so.tilo.diffmerge`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Session` is the app's `diffmerge.session` at `diffmerge/<resource>`, read with
`await ….load()`: its fields are the app's fields (read-only ones without a setter)
and its methods are its actions. A method answers what the app declares; an action
that takes longer answers a `Run`. A record is the dict the wire carries, and an
enum whose cases carry fields is an object whose tag the app's schema names. An
action whose name is one the class already has is run with `action("…")`, which
the file says where it is.
"""

from __future__ import annotations

from typing import Any, Literal, TypedDict
from tilo import Resource, Run

__all__ = [
    "Session",
]

class Session(Resource):
    """Diff & Merge — `diffmerge.session`."""
    @property
    def comparison(self) -> Any:
        """Read only."""
        return self.fields["comparison"]
    @property
    def merge(self) -> Any:
        """Read only."""
        return self.fields["merge"]
    merged: Any
    @property
    def resolutions(self) -> Any:
        """Read only."""
        return self.fields["resolutions"]
    @property
    def written(self) -> Any:
        """Read only."""
        return self.fields["written"]

    def open_comparison(self,
        *,
        left_label: str,
        left: str,
        right_label: str,
        right: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("open-comparison", left_label=left_label, left=left, right_label=right_label, right=right)

    def open_merge(self,
        *,
        base_label: str,
        base: str,
        ours_label: str,
        ours: str,
        theirs_label: str,
        theirs: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("open-merge", base_label=base_label, base=base, ours_label=ours_label, ours=ours, theirs_label=theirs_label, theirs=theirs)

    def take_ours(self, *, hunk: int) -> Run | None:
        """Runs the app's action."""
        return self.action("take-ours", hunk=hunk)

    def take_theirs(self, *, hunk: int) -> Run | None:
        """Runs the app's action."""
        return self.action("take-theirs", hunk=hunk)

    def take_both(self, *, hunk: int) -> Run | None:
        """Runs the app's action."""
        return self.action("take-both", hunk=hunk)

    def resolve(self,
        *,
        hunk: int,
        text: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("resolve", hunk=hunk, text=text)


