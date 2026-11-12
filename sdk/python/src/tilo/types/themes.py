"""Types for Theme Studio (`so.tilo.themes`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Studio` is the app's `themes.studio` at `themes/<resource>`, read with
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
    "Ink",
    "Presence",
    "Status",
    "StatusTone",
    "Surfaces",
    "Theme",
    "Studio",
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

class Studio(Resource):
    """Themes — `themes.studio`."""
    themes: list[Theme]

    def new_theme(self,
        *,
        id: str,
        base: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("new-theme", id=id, base=base)

    def import_theme(self,
        *,
        id: str,
        document: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("import-theme", id=id, document=document)

    def reset(self, *, theme: str) -> Run | None:
        """Runs the app's action."""
        return self.action("reset", theme=theme)

    def delete(self, *, theme: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete", theme=theme)

    def switch_polarity(self,
        *,
        theme: str,
        dark: bool,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("switch-polarity", theme=theme, dark=dark)

    def export(self, *, theme: str) -> None:
        """Runs the app's action."""
        return self.action("export", theme=theme)


