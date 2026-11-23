"""Types for Settings (`so.tilo.settings`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Root` is the app's `settings.root` at `settings/<resource>`, read with
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
    "ListedAccount",
    "Root",
    "Accounts",
    "PublishBrowserIdentityArgs",
    "SaveAccountArgs",
    "RemoveAccountArgs",
]

class ListedAccount(TypedDict):
    type_id: str
    display_name: str
    config: Any
    enabled: bool
    tint: int

PublishBrowserIdentityArgs = TypedDict("PublishBrowserIdentityArgs", {
    "root": "str",
    "origin": "str",
    "display_name": "str",
})
"""The arguments of `publish-browser-identity`."""

SaveAccountArgs = TypedDict("SaveAccountArgs", {
    "id": "str",
    "type_id": "str",
    "display_name": "str",
    "config": "Any",
    "enabled": "bool",
    "tint": "int",
})
"""The arguments of `save-account`."""

RemoveAccountArgs = TypedDict("RemoveAccountArgs", {
    "account": "str",
})
"""The arguments of `remove-account`."""

class Root(Resource):
    """Settings — `settings.root`."""
    __surface__ = "so.tilo.settings@2edd3868103ec032"
    def inspect_input_sources(self,
        *,
        query: str | None = None,
        page: int | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if query is not None:
            args["query"] = query
        if page is not None:
            args["page"] = page
        return self.action("inspect-input-sources", **args)

    def inspect_keyboard_map(self) -> None:
        """Runs the app's action."""
        return self.action("inspect-keyboard-map")

    def inspect_keyboard_sources(self) -> None:
        """Runs the app's action."""
        return self.action("inspect-keyboard-sources")

    def set_source_options_visible(self, *, visible: bool) -> None:
        """Runs the app's action."""
        return self.action("set-source-options-visible", visible=visible)

    def set_source_inspection_visible(self, *, visible: bool) -> None:
        """Runs the app's action."""
        return self.action("set-source-inspection-visible", visible=visible)

    def open_source_picker(self) -> None:
        """Runs the app's action."""
        return self.action("open-source-picker")

    def show_live_keyboard_map(self) -> None:
        """Runs the app's action."""
        return self.action("show-live-keyboard-map")

    def storage_summary(self) -> None:
        """Runs the app's action."""
        return self.action("storage-summary")

    def search_hub_models(self,
        *,
        query: str,
        format: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["query"] = query
        if format is not None:
            args["format"] = format
        return self.action("search-hub-models", **args)

    def show_hub_model(self, *, model: str) -> None:
        """Runs the app's action."""
        return self.action("show-hub-model", model=model)

    def toggle_microphone_test(self) -> None:
        """Runs the app's action."""
        return self.action("toggle-microphone-test")

    def inspect_calendar_choices(self) -> None:
        """Runs the app's action."""
        return self.action("inspect-calendar-choices")

    def inspect_display_configuration(self) -> None:
        """Runs the app's action."""
        return self.action("inspect-display-configuration")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"inspect_input_sources", "inspect_keyboard_map", "inspect_keyboard_sources", "set_source_options_visible", "set_source_inspection_visible", "open_source_picker", "show_live_keyboard_map", "storage_summary", "search_hub_models", "show_hub_model", "toggle_microphone_test", "inspect_calendar_choices", "inspect_display_configuration"})


class Accounts(Resource):
    """Accounts — `tilo.accounts`."""
    __surface__ = "so.tilo.settings@2edd3868103ec032"
    accounts: list[Entry[ListedAccount]]

    def publish_browser_identity(self,
        *,
        root: str,
        origin: str,
        display_name: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("publish-browser-identity", root=root, origin=origin, display_name=display_name)

    def publish_browser_identity_many(self, items: list[PublishBrowserIdentityArgs]) -> list[dict[str, Any]]:
        """Runs `publish-browser-identity` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("publish-browser-identity", items)

    def save_account(self,
        *,
        id: str,
        type_id: str,
        display_name: str,
        config: Any,
        enabled: bool,
        tint: int,
    ) -> None:
        """Runs the app's action."""
        return self.action("save-account", id=id, type_id=type_id, display_name=display_name, config=config, enabled=enabled, tint=tint)

    def save_account_many(self, items: list[SaveAccountArgs]) -> list[dict[str, Any]]:
        """Runs `save-account` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("save-account", items)

    def remove_account(self, *, account: str) -> None:
        """Runs the app's action."""
        return self.action("remove-account", account=account)

    def remove_account_many(self, items: list[RemoveAccountArgs]) -> list[dict[str, Any]]:
        """Runs `remove-account` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove-account", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"publish_browser_identity", "publish_browser_identity_many", "save_account", "save_account_many", "remove_account", "remove_account_many"})


