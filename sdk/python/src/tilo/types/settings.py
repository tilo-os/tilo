"""Types for Settings (`so.tilo.settings`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Settings` is the app's `tl.so.tilo.settings.Settings` at `settings/<resource>`, read with
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
    "Settings",
    "TiloSettings",
    "ImportThemeArgs",
    "OpenPaneArgs",
    "ShowSettingArgs",
    "ReviewContrastArgs",
    "ReviewContrastOnArgs",
    "ReviewContrastNowArgs",
]

ImportThemeArgs = TypedDict("ImportThemeArgs", {
    "file": "Any",
})
"""The arguments of `import_theme`."""

OpenPaneArgs = TypedDict("OpenPaneArgs", {
    "page": "str",
})
"""The arguments of `open_pane`."""

ShowSettingArgs = TypedDict("ShowSettingArgs", {
    "setting": "str",
})
"""The arguments of `show_setting`."""

ReviewContrastArgs = TypedDict("ReviewContrastArgs", {
    "theme": "str",
    "role": "str",
    "value": "float",
})
"""The arguments of `review_contrast`."""

ReviewContrastOnArgs = TypedDict("ReviewContrastOnArgs", {
    "theme": "str",
    "role": "str",
    "on": "bool",
})
"""The arguments of `review_contrast_on`."""

ReviewContrastNowArgs = TypedDict("ReviewContrastNowArgs", {
    "theme": "str",
})
"""The arguments of `review_contrast_now`."""

class Settings(Resource):
    """Settings — `tl.so.tilo.settings.Settings`."""
    __surface__ = "so.tilo.settings@b91ff8fb98358e8b"
    vfi4twig: str  # pane

    def import_theme(self, *, file: Any) -> None:
        """Takes in a theme saved with Export, from this workspace or another: every role, tuned value and glass shader, once it is admitted and keeps every contrast floor. Importing names it by its own name, so what a person exported under a name comes back under one."""
        return self.action("import_theme", file=file)

    def import_theme_many(self, items: list[ImportThemeArgs]) -> list[dict[str, Any]]:
        """Runs `import_theme` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import_theme", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"import_theme", "import_theme_many"})


class TiloSettings(Resource):
    """Settings — `tl.so.tilo.settings`."""
    __surface__ = "so.tilo.settings@b91ff8fb98358e8b"
    def move_screen(self,
        *,
        screens: Any,
        picked: int,
        start: list[float],
        let_go: list[float],
    ) -> None:
        """Move the screen picked to where the pointer was let go, in the desktop's own pixels, snapped to a small grid: a drag too small to mean anything is not a change."""
        return self.action("move_screen", screens=screens, picked=picked, start=start, let_go=let_go)

    def open_pane(self, *, page: str) -> None:
        """Show the pane a search page names, as the search's own row runs it from outside the window."""
        return self.action("open_pane", page=page)

    def open_pane_many(self, items: list[OpenPaneArgs]) -> list[dict[str, Any]]:
        """Runs `open_pane` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("open_pane", items)

    def show_setting(self, *, setting: str) -> None:
        """Show the pane a setting is on, as search's own row for a setting runs it."""
        return self.action("show_setting", setting=setting)

    def show_setting_many(self, items: list[ShowSettingArgs]) -> list[dict[str, Any]]:
        """Runs `show_setting` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show_setting", items)

    def set_processors_room(self, *, percent: float) -> None:
        """The processors this device keeps free for the people at it: the slider's share, made whole."""
        return self.action("set_processors_room", percent=percent)

    def set_memory_room(self, *, percent: float) -> None:
        """The memory this device keeps free for the people at it."""
        return self.action("set_memory_room", percent=percent)

    def review_contrast(self,
        *,
        theme: str,
        role: str,
        value: float,
    ) -> None:
        """Lists every contrast floor the theme would miss with `role` at the control's value, so its editor shows each pairing under its floor while the theme is tuned. The theme keeps the roles it admits; a value that would break a floor is refused, and this says which pairs it would break."""
        return self.action("review_contrast", theme=theme, role=role, value=value)

    def review_contrast_many(self, items: list[ReviewContrastArgs]) -> list[dict[str, Any]]:
        """Runs `review_contrast` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("review_contrast", items)

    def review_contrast_on(self,
        *,
        theme: str,
        role: str,
        on: bool,
    ) -> None:
        """The same after a switch was tried: its on is `1` and its off `0`."""
        return self.action("review_contrast_on", theme=theme, role=role, on=on)

    def review_contrast_on_many(self, items: list[ReviewContrastOnArgs]) -> list[dict[str, Any]]:
        """Runs `review_contrast_on` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("review_contrast_on", items)

    def review_contrast_now(self, *, theme: str) -> None:
        """Records what the theme misses as it stands, when its editor opens or its polarity changes, before any control has carried a value."""
        return self.action("review_contrast_now", theme=theme)

    def review_contrast_now_many(self, items: list[ReviewContrastNowArgs]) -> list[dict[str, Any]]:
        """Runs `review_contrast_now` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("review_contrast_now", items)

    def export_theme(self, *, t: Any) -> Any:
        """The theme as a file: what exporting it saves, and importing it on another workspace reads."""
        return self.action("export_theme", t=t)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"move_screen", "open_pane", "open_pane_many", "show_setting", "show_setting_many", "set_processors_room", "set_memory_room", "review_contrast", "review_contrast_many", "review_contrast_on", "review_contrast_on_many", "review_contrast_now", "review_contrast_now_many", "export_theme"})


