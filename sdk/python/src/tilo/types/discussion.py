"""Types for Discussion (`so.tilo.lang.discussion`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Comment` is the app's `tl.so.tilo.lang.discussion.Comment` at `discussion/<resource>`, read with
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
    "Comment",
    "Comments",
    "Discussion",
    "DeleteArgs",
]

DeleteArgs = TypedDict("DeleteArgs", {
    "c": "Comment",
})
"""The arguments of `delete`."""

class Comment(Resource):
    """Comment — `tl.so.tilo.lang.discussion.Comment`."""
    __surface__ = "so.tilo.lang.discussion@3dc800efbd65ec71"
    @property
    def _5q4mbecw(self) -> str:
        """text (`5q4mbecw` on the wire)."""
        return self.fields["5q4mbecw"]

    @_5q4mbecw.setter
    def _5q4mbecw(self, value: str) -> None:
        self.edit("5q4mbecw", value=value)

    # the app's `edit` action is the SDK's own: run it with `action("edit")`.

class Comments(Resource):
    """Comments — `tl.so.tilo.lang.discussion.Comments`."""
    __surface__ = "so.tilo.lang.discussion@3dc800efbd65ec71"
    @property
    def _3g3duquj(self) -> list[Entry[Comment]]:
        """items (`3g3duquj` on the wire)."""
        return self.fields["3g3duquj"]

    @_3g3duquj.setter
    def _3g3duquj(self, value: list[Entry[Comment]]) -> None:
        self.edit("3g3duquj", value=value)

    def comment(self, *, text: str) -> Comment:
        """Says something about the resource the comment is made from: the thread under a task, a note or a mail says it with `.made_from(r)`, so the comment records what it is about. An agent says something the same way, its comment its own as any person's is."""
        return self.action("comment", text=text)

    def delete(self, *, c: Comment) -> None:
        """Deletes a comment. Only its author may. The platform takes it back rather than asking first, and the removal is a version of the comment, so who removed it stays answerable."""
        return self.action("delete", c=c)

    def delete_many(self, items: list[DeleteArgs]) -> list[dict[str, Any]]:
        """Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"comment", "delete", "delete_many"})


class Discussion(Resource):
    """Discussion — `tl.so.tilo.lang.discussion`."""
    __surface__ = "so.tilo.lang.discussion@3dc800efbd65ec71"

