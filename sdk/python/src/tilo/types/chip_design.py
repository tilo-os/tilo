"""Types for Chip Design (`so.tilo.chip-design`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Project` is the app's `chip_design.project` at `chip-design/<resource>`, read with
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
    "Connection",
    "OperatorKind",
    "OperatorNode",
    "Project",
    "ImportReportArgs",
    "RemoveReportArgs",
    "ImportVerificationArgs",
    "RemoveVerificationArgs",
    "AddNodeArgs",
    "SetShapeArgs",
    "RemoveNodeArgs",
    "ConnectArgs",
    "DisconnectArgs",
]

class Connection(TypedDict):
    from_node: str
    from_port: str
    to_node: str
    to_port: str

OperatorKind = Literal["input", "gate_input", "norm_weight_input", "rms_norm", "apply_norm_weight", "silu", "multiply", "projection_input", "matrix_weight_input", "linear_projection", "add"]

class OperatorNode(TypedDict):
    id: str
    kind: OperatorKind
    shape: list[int]
    normalization_width: int
    output_width: int | None
    x: int
    y: int

ImportReportArgs = TypedDict("ImportReportArgs", {
    "json": "str",
})
"""The arguments of `import-report`."""

RemoveReportArgs = TypedDict("RemoveReportArgs", {
    "report": "str",
})
"""The arguments of `remove-report`."""

ImportVerificationArgs = TypedDict("ImportVerificationArgs", {
    "json": "str",
})
"""The arguments of `import-verification`."""

RemoveVerificationArgs = TypedDict("RemoveVerificationArgs", {
    "report": "str",
})
"""The arguments of `remove-verification`."""

AddNodeArgs = TypedDict("AddNodeArgs", {
    "node": "str",
    "kind": "OperatorKind",
    "shape": "list[int]",
    "output_width": NotRequired["int | None"],
    "x": "int",
    "y": "int",
})
"""The arguments of `add-node`."""

SetShapeArgs = TypedDict("SetShapeArgs", {
    "node": "str",
    "shape": "list[int]",
})
"""The arguments of `set-shape`."""

RemoveNodeArgs = TypedDict("RemoveNodeArgs", {
    "node": "str",
})
"""The arguments of `remove-node`."""

ConnectArgs = TypedDict("ConnectArgs", {
    "from_node": "str",
    "from_port": "str",
    "to_node": "str",
    "to_port": "str",
})
"""The arguments of `connect`."""

DisconnectArgs = TypedDict("DisconnectArgs", {
    "node": "str",
    "port": "str",
})
"""The arguments of `disconnect`."""

class Project(Resource):
    """Chip Design — `chip_design.project`."""
    __surface__ = "so.tilo.chip-design@298902c837db4f1e"
    @property
    def reports(self) -> dict[str, str]:
        """Read only."""
        return self.fields["reports"]
    @property
    def verifications(self) -> dict[str, str]:
        """Read only."""
        return self.fields["verifications"]
    nodes: list[Entry[OperatorNode]]
    @property
    def connections(self) -> list[Connection]:
        """Read only."""
        return self.fields["connections"]

    def import_report(self, *, json: str) -> None:
        """Runs the app's action."""
        return self.action("import-report", json=json)

    def import_report_many(self, items: list[ImportReportArgs]) -> list[dict[str, Any]]:
        """Runs `import-report` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import-report", items)

    def remove_report(self, *, report: str) -> None:
        """Runs the app's action."""
        return self.action("remove-report", report=report)

    def remove_report_many(self, items: list[RemoveReportArgs]) -> list[dict[str, Any]]:
        """Runs `remove-report` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove-report", items)

    def import_verification(self, *, json: str) -> None:
        """Runs the app's action."""
        return self.action("import-verification", json=json)

    def import_verification_many(self, items: list[ImportVerificationArgs]) -> list[dict[str, Any]]:
        """Runs `import-verification` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import-verification", items)

    def remove_verification(self, *, report: str) -> None:
        """Runs the app's action."""
        return self.action("remove-verification", report=report)

    def remove_verification_many(self, items: list[RemoveVerificationArgs]) -> list[dict[str, Any]]:
        """Runs `remove-verification` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove-verification", items)

    def add_node(self,
        *,
        node: str,
        kind: OperatorKind,
        shape: list[int],
        x: int,
        y: int,
        output_width: int | None = None,
    ) -> None:
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

    def add_node_many(self, items: list[AddNodeArgs]) -> list[dict[str, Any]]:
        """Runs `add-node` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-node", items)

    def set_shape(self,
        *,
        node: str,
        shape: list[int],
    ) -> None:
        """Runs the app's action."""
        return self.action("set-shape", node=node, shape=shape)

    def set_shape_many(self, items: list[SetShapeArgs]) -> list[dict[str, Any]]:
        """Runs `set-shape` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-shape", items)

    def remove_node(self, *, node: str) -> None:
        """Runs the app's action."""
        return self.action("remove-node", node=node)

    def remove_node_many(self, items: list[RemoveNodeArgs]) -> list[dict[str, Any]]:
        """Runs `remove-node` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove-node", items)

    def connect(self,
        *,
        from_node: str,
        from_port: str,
        to_node: str,
        to_port: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("connect", from_node=from_node, from_port=from_port, to_node=to_node, to_port=to_port)

    def connect_many(self, items: list[ConnectArgs]) -> list[dict[str, Any]]:
        """Runs `connect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("connect", items)

    def disconnect(self,
        *,
        node: str,
        port: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("disconnect", node=node, port=port)

    def disconnect_many(self, items: list[DisconnectArgs]) -> list[dict[str, Any]]:
        """Runs `disconnect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("disconnect", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"import_report", "import_report_many", "remove_report", "remove_report_many", "import_verification", "import_verification_many", "remove_verification", "remove_verification_many", "add_node", "add_node_many", "set_shape", "set_shape_many", "remove_node", "remove_node_many", "connect", "connect_many", "disconnect", "disconnect_many"})


