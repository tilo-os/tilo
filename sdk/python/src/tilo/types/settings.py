"""Types for Settings (`so.tilo.settings`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `settings/<resource>`, read with
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
    "Notice",
    "Settings",
    "ProblemReport",
    "TiloSettings",
    "ImportThemeArgs",
    "MoveSharedScreenArgs",
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

MoveSharedScreenArgs = TypedDict("MoveSharedScreenArgs", {
    "night": "Any",
    "screens": "list[Any]",
    "picked": "int",
    "start": "list[float]",
    "let_go": "list[float]",
})
"""The arguments of `move_shared_screen`."""

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

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.settings@4968411ed7e719ce"
    @property
    def notice_to(self) -> str:
        """to (`notice-to` on the wire)."""
        return self.fields["notice-to"]

    @notice_to.setter
    def notice_to(self, value: str) -> None:
        self.edit("notice-to", value=value)
    @property
    def notice_title(self) -> str:
        """title (`notice-title` on the wire)."""
        return self.fields["notice-title"]

    @notice_title.setter
    def notice_title(self, value: str) -> None:
        self.edit("notice-title", value=value)
    @property
    def notice_body(self) -> str:
        """body (`notice-body` on the wire)."""
        return self.fields["notice-body"]

    @notice_body.setter
    def notice_body(self, value: str) -> None:
        self.edit("notice-body", value=value)
    @property
    def notice_opens(self) -> Any:
        """opens (`notice-opens` on the wire)."""
        return self.fields["notice-opens"]

    @notice_opens.setter
    def notice_opens(self, value: Any) -> None:
        self.edit("notice-opens", value=value)
    @property
    def notice_seen(self) -> bool:
        """seen (`notice-seen` on the wire)."""
        return self.fields["notice-seen"]

    @notice_seen.setter
    def notice_seen(self, value: bool) -> None:
        self.edit("notice-seen", value=value)
    @property
    def notice_device(self) -> str:
        """device (`notice-device` on the wire)."""
        return self.fields["notice-device"]

    @notice_device.setter
    def notice_device(self, value: str) -> None:
        self.edit("notice-device", value=value)
    @property
    def notice_actions(self) -> list[str]:
        """actions (`notice-actions` on the wire)."""
        return self.fields["notice-actions"]

    @notice_actions.setter
    def notice_actions(self, value: list[str]) -> None:
        self.edit("notice-actions", value=value)


class Settings(Resource):
    """Settings — `tl.so.tilo.settings.Settings`."""
    __surface__ = "so.tilo.settings@4968411ed7e719ce"
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


class ProblemReport(Resource):
    """ProblemReport — `tl.so.tilo.settings.ProblemReport`."""
    __surface__ = "so.tilo.settings@4968411ed7e719ce"
    kb2c7b7e: str  # body


class TiloSettings(Resource):
    """Settings — `tl.so.tilo.settings`."""
    __surface__ = "so.tilo.settings@4968411ed7e719ce"
    def move_shared_screen(self,
        *,
        night: Any,
        screens: list[Any],
        picked: int,
        start: list[float],
        let_go: list[float],
    ) -> None:
        """Move the device whose screen was dragged so that screen stands where the pointer was let go, in the shared space's own pixels, snapped to a small grid. Every screen of that device moves with it; a drag too small to mean anything is not a change."""
        return self.action("move_shared_screen", night=night, screens=screens, picked=picked, start=start, let_go=let_go)

    def move_shared_screen_many(self, items: list[MoveSharedScreenArgs]) -> list[dict[str, Any]]:
        """Runs `move_shared_screen` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_shared_screen", items)

    def move_screen(self,
        *,
        screens: Any,
        picked: int,
        start: list[float],
        let_go: list[float],
    ) -> None:
        """Move the screen picked to where the pointer was let go, in the desktop's own pixels, snapped to a small grid: a drag too small to mean anything is not a change."""
        return self.action("move_screen", screens=screens, picked=picked, start=start, let_go=let_go)

    def set_sdr_brightness(self,
        *,
        displays: Any,
        display: str,
        nits: float,
    ) -> None:
        """Set how bright ordinary content looks in HDR, from the slider's value in cd/m²."""
        return self.action("set_sdr_brightness", displays=displays, display=display, nits=nits)

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

    def open_manage_page(self, *, account: Any) -> None:
        """Opens the provider's page where a card, a plan and the invoices change, in the app that opens web addresses: the provider's own page, never a form Tilo draws. The page is the one the account names, so nothing an app or an agent passes can open another, and an account that names none opens nothing."""
        return self.action("open_manage_page", account=account)

    def open_invoice(self,
        *,
        account: Any,
        invoice: str,
    ) -> None:
        """Opens one of the account's invoices on the provider's page. The invoice is the account's own, looked up by its id, so only a page the account names opens and an id it does not hold opens nothing."""
        return self.action("open_invoice", account=account, invoice=invoice)

    def set_processors_room(self, *, percent: float) -> None:
        """The processors this device keeps free for the people at it: the slider's share, made whole."""
        return self.action("set_processors_room", percent=percent)

    def set_memory_room(self, *, percent: float) -> None:
        """The memory this device keeps free for the people at it."""
        return self.action("set_memory_room", percent=percent)

    def send_problem_report(self, *, body: str) -> None:
        """Files this computer's record of a problem as a problem report in the workspace's feedback channel, where the people who make Tilo read what went wrong, with the record's own words."""
        return self.action("send_problem_report", body=body)

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
    __actions__ = frozenset({"move_shared_screen", "move_shared_screen_many", "move_screen", "set_sdr_brightness", "open_pane", "open_pane_many", "show_setting", "show_setting_many", "open_manage_page", "open_invoice", "set_processors_room", "set_memory_room", "send_problem_report", "review_contrast", "review_contrast_many", "review_contrast_on", "review_contrast_on_many", "review_contrast_now", "review_contrast_now_many", "export_theme"})


