"""Types for Calculator (`so.tilo.calculator`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Calculation` is the app's `tl.so.tilo.calculator.Calculation` at `calculator/<resource>`, read with
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
    "Calculation",
    "Curve",
    "Surface",
    "Calculator",
    "TiloCalculator",
    "ForgetArgs",
    "DeleteCurveArgs",
    "DeleteSurfaceArgs",
    "ClearArgs",
]

ForgetArgs = TypedDict("ForgetArgs", {
})
"""The arguments of `forget`."""

DeleteCurveArgs = TypedDict("DeleteCurveArgs", {
})
"""The arguments of `delete_curve`."""

DeleteSurfaceArgs = TypedDict("DeleteSurfaceArgs", {
})
"""The arguments of `delete_surface`."""

ClearArgs = TypedDict("ClearArgs", {
})
"""The arguments of `clear`."""

class Calculation(Resource):
    """Calculation — `tl.so.tilo.calculator.Calculation`."""
    __surface__ = "so.tilo.calculator@4adf6e79cc5a52de"
    @property
    def _22vu26uc(self) -> str:
        """expr (`22vu26uc` on the wire)."""
        return self.fields["22vu26uc"]

    @_22vu26uc.setter
    def _22vu26uc(self, value: str) -> None:
        self.edit("22vu26uc", value=value)
    y3tbavz4: str  # result

    def forget(self) -> None:
        """Takes one calculation out of the record."""
        return self.action("forget")

    def forget_many(self, items: list[ForgetArgs]) -> list[dict[str, Any]]:
        """Runs `forget` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("forget", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"forget", "forget_many"})


class Curve(Resource):
    """Curve — `tl.so.tilo.calculator.Curve`."""
    __surface__ = "so.tilo.calculator@4adf6e79cc5a52de"
    pxeb7d43: str  # expr
    @property
    def _5jdf7feh(self) -> bool:
        """visible (`5jdf7feh` on the wire)."""
        return self.fields["5jdf7feh"]

    @_5jdf7feh.setter
    def _5jdf7feh(self, value: bool) -> None:
        self.edit("5jdf7feh", value=value)

    def delete_curve(self) -> None:
        """Takes a curve off the 2D graph."""
        return self.action("delete_curve")

    def delete_curve_many(self, items: list[DeleteCurveArgs]) -> list[dict[str, Any]]:
        """Runs `delete_curve` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_curve", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"delete_curve", "delete_curve_many"})


class Surface(Resource):
    """Surface — `tl.so.tilo.calculator.Surface`."""
    __surface__ = "so.tilo.calculator@4adf6e79cc5a52de"
    ia2fz4sd: str  # expr
    iuelnno5: bool  # visible

    def delete_surface(self) -> None:
        """Takes a surface off the 3D graph."""
        return self.action("delete_surface")

    def delete_surface_many(self, items: list[DeleteSurfaceArgs]) -> list[dict[str, Any]]:
        """Runs `delete_surface` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_surface", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"delete_surface", "delete_surface_many"})


class Calculator(Resource):
    """Calculator — `tl.so.tilo.calculator.Calculator`."""
    __surface__ = "so.tilo.calculator@4adf6e79cc5a52de"
    tb3paq5e: list[Entry[Calculation]]  # calculations
    @property
    def _7h4yz4ro(self) -> list[Entry[Curve]]:
        """curves (`7h4yz4ro` on the wire)."""
        return self.fields["7h4yz4ro"]

    @_7h4yz4ro.setter
    def _7h4yz4ro(self, value: list[Entry[Curve]]) -> None:
        self.edit("7h4yz4ro", value=value)
    zdfqnd3x: list[Entry[Surface]]  # surfaces

    def calculate(self,
        *,
        expr: str,
        in_degrees: bool | None = None,
        answer: float | None = None,
        memory: float | None = None,
    ) -> Calculation:
        """Works out an expression and keeps the result in the record. The result is checked here, where the log records it with the expression that made it, and the display says the same number."""
        args: dict[str, Any] = {}
        args["expr"] = expr
        if in_degrees is not None:
            args["in_degrees"] = in_degrees
        if answer is not None:
            args["answer"] = answer
        if memory is not None:
            args["memory"] = memory
        return self.action("calculate", **args)

    def add_curve(self,
        *,
        formula: str | None = None,
    ) -> Curve:
        """Adds a curve to the 2D graph."""
        args: dict[str, Any] = {}
        if formula is not None:
            args["formula"] = formula
        return self.action("add_curve", **args)

    def add_surface(self,
        *,
        formula: str | None = None,
    ) -> Surface:
        """Adds a surface to the 3D graph."""
        args: dict[str, Any] = {}
        if formula is not None:
            args["formula"] = formula
        return self.action("add_surface", **args)

    def clear(self) -> None:
        """Clears the record of everything worked out so far."""
        return self.action("clear")

    def clear_many(self, items: list[ClearArgs]) -> list[dict[str, Any]]:
        """Runs `clear` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"calculate", "add_curve", "add_surface", "clear", "clear_many"})


class TiloCalculator(Resource):
    """Calculator — `tl.so.tilo.calculator`."""
    __surface__ = "so.tilo.calculator@4adf6e79cc5a52de"
    def copy_text(self, *, text: str) -> None:
        """Puts `text` on the person's clipboard, as the system's copy does."""
        return self.action("copy_text", text=text)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"copy_text"})


