"""Types for Print Centre (`so.tilo.print-centre`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Defaults` is the app's `print-centre.defaults` at `print-centre/<resource>`, read with
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
    "Defaults",
]

class Defaults(Resource):
    """Print Centre — `print-centre.defaults`."""
    two_sided: bool
    scan_resolution: int
    scan_color: str
    scan_source: str
    ocr_language: str

    # the app's `refresh` action is the SDK's own: run it with `action("refresh")`.

