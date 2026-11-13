"""Types for Disk Usage Analyzer (`so.tilo.disk-usage`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`DiskUsage` is the app's `disk-usage` at `disk-usage/<resource>`, read with
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
    "DiskUsage",
    "RecentFolders",
]

class DiskUsage(Resource):
    """Disk Usage Analyzer — `disk-usage`."""
    __surface__ = "so.tilo.disk-usage@142cf5e7136a5290"

class RecentFolders(Resource):
    """Recent folders — `disk-usage.recent-folders`."""
    __surface__ = "so.tilo.disk-usage@142cf5e7136a5290"
    devices: dict[str, Any]

    def remember(self,
        *,
        device: str,
        folder: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("remember", device=device, folder=folder)


