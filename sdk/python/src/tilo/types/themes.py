"""Types for Theme Studio (`so.tilo.themes`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Studio` is the app's `themes.studio` at `themes/<resource>`, read with
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
    "Ink",
    "Presence",
    "Status",
    "StatusTone",
    "Surfaces",
    "Theme",
    "Studio",
    "NewThemeArgs",
    "ImportThemeArgs",
    "ResetArgs",
    "DeleteArgs",
    "SwitchPolarityArgs",
]

class Ink(TypedDict):
    primary: float
    secondary: float
    tertiary: float

class Presence(TypedDict):
    lightness: float
    chroma: float

class Status(TypedDict):
    danger: StatusTone
    warning: StatusTone
    success: StatusTone
    info: StatusTone

class StatusTone(TypedDict):
    lightness: float
    chroma: float

class Surfaces(TypedDict):
    base: float
    content: float
    sidebar: float
    inspector: float
    raised: float
    overlay: float

class Theme(TypedDict):
    name: str
    dark: bool
    high_contrast: bool
    hue: float | None
    chroma: float
    surfaces: Surfaces
    ink: Ink
    status: Status
    tint: float | None
    monochrome: bool
    presence: Presence
    window_radius: float
    density: float
    face: str
    base_size: float
    distinct_letters: bool | None
    motion: float
    blur: float
    tint_opacity: float
    frosted_strip: bool
    rim_span: float
    rim_alpha: float
    hover_alpha: float
    origin: str | None

NewThemeArgs = TypedDict("NewThemeArgs", {
    "id": "str",
    "base": "str",
})
"""The arguments of `new-theme`."""

ImportThemeArgs = TypedDict("ImportThemeArgs", {
    "id": "str",
    "document": "str",
})
"""The arguments of `import-theme`."""

ResetArgs = TypedDict("ResetArgs", {
    "theme": "str",
})
"""The arguments of `reset`."""

DeleteArgs = TypedDict("DeleteArgs", {
    "theme": "str",
})
"""The arguments of `delete`."""

SwitchPolarityArgs = TypedDict("SwitchPolarityArgs", {
    "theme": "str",
    "dark": "bool",
})
"""The arguments of `switch-polarity`."""

class Studio(Resource):
    """Themes — `themes.studio`."""
    __surface__ = "so.tilo.themes@5e3310065e4c4d56"
    themes: list[Entry[Theme]]

    def new_theme(self,
        *,
        id: str,
        base: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("new-theme", id=id, base=base)

    def new_theme_many(self, items: list[NewThemeArgs]) -> list[dict[str, Any]]:
        """Runs `new-theme` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("new-theme", items)

    def import_theme(self,
        *,
        id: str,
        document: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("import-theme", id=id, document=document)

    def import_theme_many(self, items: list[ImportThemeArgs]) -> list[dict[str, Any]]:
        """Runs `import-theme` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import-theme", items)

    def reset(self, *, theme: str) -> None:
        """Runs the app's action."""
        return self.action("reset", theme=theme)

    def reset_many(self, items: list[ResetArgs]) -> list[dict[str, Any]]:
        """Runs `reset` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("reset", items)

    def delete(self, *, theme: str) -> None:
        """Runs the app's action."""
        return self.action("delete", theme=theme)

    def delete_many(self, items: list[DeleteArgs]) -> list[dict[str, Any]]:
        """Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete", items)

    def switch_polarity(self,
        *,
        theme: str,
        dark: bool,
    ) -> None:
        """Runs the app's action."""
        return self.action("switch-polarity", theme=theme, dark=dark)

    def switch_polarity_many(self, items: list[SwitchPolarityArgs]) -> list[dict[str, Any]]:
        """Runs `switch-polarity` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("switch-polarity", items)

    def export(self, *, theme: str) -> None:
        """Runs the app's action."""
        return self.action("export", theme=theme)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_theme", "new_theme_many", "import_theme", "import_theme_many", "reset", "reset_many", "delete", "delete_many", "switch_polarity", "switch_polarity_many", "export"})


