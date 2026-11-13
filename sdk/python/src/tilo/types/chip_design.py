"""Types for Chip Design (`so.tilo.chip-design`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Project` is the app's `chip_design.project` at `chip-design/<resource>`, read with
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
    "Project",
]

class Project(Resource):
    """Chip Design — `chip_design.project`."""
    __surface__ = "so.tilo.chip-design@994cb5cd1f2f6a7b"
    @property
    def reports(self) -> Any:
        """Read only."""
        return self.fields["reports"]
    @property
    def verifications(self) -> Any:
        """Read only."""
        return self.fields["verifications"]
    nodes: Any
    @property
    def connections(self) -> Any:
        """Read only."""
        return self.fields["connections"]

    def import_report(self, *, json: str) -> Run | None:
        """Runs the app's action."""
        return self.action("import-report", json=json)

    def remove_report(self, *, report: str) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-report", report=report)

    def import_verification(self, *, json: str) -> Run | None:
        """Runs the app's action."""
        return self.action("import-verification", json=json)

    def remove_verification(self, *, report: str) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-verification", report=report)

    def add_node(self,
        *,
        node: str,
        kind: str,
        shape: list[int],
        x: int,
        y: int,
        output_width: int | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["node"] = node
        args["kind"] = kind
        args["shape"] = shape
        if output_width is not None:
            args["output_width"] = output_width
        args["x"] = x
        args["y"] = y
        return self.action("add-node", **args)

    def set_shape(self,
        *,
        node: str,
        shape: list[int],
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-shape", node=node, shape=shape)

    def remove_node(self, *, node: str) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-node", node=node)

    def connect(self,
        *,
        from_node: str,
        from_port: str,
        to_node: str,
        to_port: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("connect", from_node=from_node, from_port=from_port, to_node=to_node, to_port=to_port)

    def disconnect(self,
        *,
        node: str,
        port: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("disconnect", node=node, port=port)


