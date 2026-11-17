"""Types for Speed Test (`so.tilo.speed-test`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Results` is the app's `speed-test.results` at `speed-test/<resource>`, read with
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
    "Measurement",
    "Results",
    "RecordArgs",
    "ClearArgs",
]

class Measurement(TypedDict):
    at_ms: int
    ping: float
    jitter: float
    download: float
    upload: float
    server: str
    device: str

RecordArgs = TypedDict("RecordArgs", {
    "ping": "float",
    "jitter": "float",
    "download": "float",
    "upload": "float",
    "server": "str",
    "device": "str",
})
"""The arguments of `record`."""

ClearArgs = TypedDict("ClearArgs", {
})
"""The arguments of `clear`."""

class Results(Resource):
    """Internet speed test — `speed-test.results`."""
    __surface__ = "so.tilo.speed-test@303db65a9e1defad"
    @property
    def results(self) -> list[Entry[Measurement]]:
        """Read only."""
        return self.fields["results"]

    def record(self,
        *,
        ping: float,
        jitter: float,
        download: float,
        upload: float,
        server: str,
        device: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("record", ping=ping, jitter=jitter, download=download, upload=upload, server=server, device=device)

    def record_many(self, items: list[RecordArgs]) -> list[dict[str, Any]]:
        """Runs `record` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("record", items)

    def clear(self) -> None:
        """Runs the app's action."""
        return self.action("clear")

    def clear_many(self, items: list[ClearArgs]) -> list[dict[str, Any]]:
        """Runs `clear` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"record", "record_many", "clear", "clear_many"})


