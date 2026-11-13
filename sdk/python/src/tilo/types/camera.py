"""Types for Camera (`so.tilo.camera`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Camera` is the app's `camera` at `camera/<resource>`, read with
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
    "CaptureKind",
    "Facing",
    "ScanEntry",
    "SharedCapture",
    "Camera",
]

CaptureKind = Literal["photo", "video"]

Facing = Literal["back", "front", "wide"]

class ScanEntry(TypedDict):
    raw: str
    ts: int

class SharedCapture(TypedDict):
    id: str
    media_id: str
    kind: CaptureKind
    ts: int
    w: int
    h: int
    dur_ms: int
    label: str
    facing: Facing

class Camera(Resource):
    """Camera — `camera`."""
    __surface__ = "so.tilo.camera@334a749412a4e922"
    @property
    def scans(self) -> list[ScanEntry]:
        """Read only."""
        return self.fields["scans"]
    @property
    def recent(self) -> SharedCapture | None:
        """Read only."""
        return self.fields["recent"]

    def register_capture(self, *, capture: SharedCapture) -> Run | None:
        """Runs the app's action."""
        return self.action("register-capture", capture=capture)

    def record_scan(self, *, raw: str) -> Run | None:
        """Runs the app's action."""
        return self.action("record-scan", raw=raw)


