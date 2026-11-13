"""Types for Speed Test (`so.tilo.speed-test`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Results` is the app's `speed-test.results` at `speed-test/<resource>`, read with
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
    "Measurement",
    "Results",
]

class Measurement(TypedDict):
    at_ms: int
    ping: float
    jitter: float
    download: float
    upload: float
    server: str
    device: str

class Results(Resource):
    """Internet speed test — `speed-test.results`."""
    __surface__ = "so.tilo.speed-test@303db65a9e1defad"
    @property
    def results(self) -> list[Measurement]:
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
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("record", ping=ping, jitter=jitter, download=download, upload=upload, server=server, device=device)

    def clear(self) -> Run | None:
        """Runs the app's action."""
        return self.action("clear")


