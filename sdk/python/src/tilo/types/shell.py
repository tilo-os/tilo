"""Types for Shell (`so.tilo.shell`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Shell` is the app's `tl.so.tilo.shell.Shell` at `shell/<resource>`, read with
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
    "BarPartKind",
    "ChromePreset",
    "Edge",
    "Finish",
    "Fit",
    "Showing",
    "Size",
    "Shell",
    "BarPart",
    "DockItem",
    "Layout",
    "LayoutQuery",
    "LayoutRef",
    "LayoutWorkspace",
    "LayoutPerson",
    "LayoutRow",
    "LayoutStrip",
    "LayoutPlace",
    "Viewing",
    "TiloShell",
    "AskedOnCanvasArgs",
    "ApplyArgs",
    "ShowAppMenusArgs",
    "HideAppMenusArgs",
    "ResetArgs",
    "BarOnArgs",
    "SetBarFitArgs",
    "SetBarFinishArgs",
    "SetBarShowingArgs",
    "SetBarSizeArgs",
    "ShowBarArgs",
    "DockOnArgs",
    "SetDockFitArgs",
    "SetDockFinishArgs",
    "SetDockShowingArgs",
    "SetDockSizeArgs",
    "ShowDockArgs",
    "SetDockRunningArgs",
    "SetDockLauncherArgs",
    "SetDockZoomArgs",
    "MovePartArgs",
    "PlacePartArgs",
    "ShowPartArgs",
    "MoveItemArgs",
    "PlaceItemArgs",
    "PinArgs",
    "UnpinArgs",
    "OpenStyleSettingsArgs",
    "CloseStyleSettingsArgs",
    "ShowStyleSettingsArgs",
    "SetStripsTabsArgs",
    "SetFramesSnapArgs",
    "TabPlaceHereArgs",
    "ReorderPlaceArgs",
    "BringForwardArgs",
    "ShowStyleArgs",
    "SizePlaceArgs",
    "PlaceDroppedArgs",
    "QueryShowArgs",
    "QueryRetryArgs",
    "ShowPlaceArgs",
]

BarPartKind = Literal["menu", "workspace", "app_menu", "strip", "tasks", "clock", "people", "status", "power", "agent", "spacer"]
"""One part a person can put on the top bar, in their order. The case's icon is the glyph the settings and the drag ghost show for it."""

ChromePreset = Literal["tilo", "macos", "windows", "gnome", "minimal", "windows_desktop"]
"""A named set of the settings, which the person applies in one action: a chrome preset. `Viewing.style` (layout.tl) is the *view* — how the same places are shown — and is a different setting; this one names the bar and the dock."""

Edge = Literal["top", "bottom", "left", "right"]
"""The person's shell: which parts the top bar and the dock have, in what order, on which edge, how they look and when they show. It is the person's own document, synced to their devices with history and undo; every change is one of the actions below, so an agent acting for the person changes it exactly as they do, and undo takes any of it back. shell.md's model: a shell is a view of the person's layout. This document is the layout's chrome — the bar and the dock — and the rest of the person's layout (windows, strips, canvas places) is the shell's own layout beside it. The edge of the screen a part rides."""

Finish = Literal["translucent", "solid"]
"""What a bar or dock is made of: the theme's translucent material over what is behind it, or its opaque bar ground."""

Fit = Literal["floating", "full"]
"""How a bar or dock sits in its edge: inset with rounded corners, floating over what it covers, or edge to edge and square."""

Showing = Literal["always", "no_overlap", "edge_reveal", "auto_hide"]
"""When a bar or dock shows."""

Size = Literal["small", "regular", "large"]
"""How large the parts are."""

AskedOnCanvasArgs = TypedDict("AskedOnCanvasArgs", {
})
"""The arguments of `asked_on_canvas`."""

ApplyArgs = TypedDict("ApplyArgs", {
    "layout": "ChromePreset",
})
"""The arguments of `apply`."""

ShowAppMenusArgs = TypedDict("ShowAppMenusArgs", {
})
"""The arguments of `show_app_menus`."""

HideAppMenusArgs = TypedDict("HideAppMenusArgs", {
})
"""The arguments of `hide_app_menus`."""

ResetArgs = TypedDict("ResetArgs", {
})
"""The arguments of `reset`."""

BarOnArgs = TypedDict("BarOnArgs", {
    "edge": "Edge",
})
"""The arguments of `bar_on`."""

SetBarFitArgs = TypedDict("SetBarFitArgs", {
    "fit": "Fit",
})
"""The arguments of `set_bar_fit`."""

SetBarFinishArgs = TypedDict("SetBarFinishArgs", {
    "finish": "Finish",
})
"""The arguments of `set_bar_finish`."""

SetBarShowingArgs = TypedDict("SetBarShowingArgs", {
    "showing": "Showing",
})
"""The arguments of `set_bar_showing`."""

SetBarSizeArgs = TypedDict("SetBarSizeArgs", {
    "size": "Size",
})
"""The arguments of `set_bar_size`."""

ShowBarArgs = TypedDict("ShowBarArgs", {
    "on": NotRequired["bool | None"],
})
"""The arguments of `show_bar`."""

DockOnArgs = TypedDict("DockOnArgs", {
    "edge": "Edge",
})
"""The arguments of `dock_on`."""

SetDockFitArgs = TypedDict("SetDockFitArgs", {
    "fit": "Fit",
})
"""The arguments of `set_dock_fit`."""

SetDockFinishArgs = TypedDict("SetDockFinishArgs", {
    "finish": "Finish",
})
"""The arguments of `set_dock_finish`."""

SetDockShowingArgs = TypedDict("SetDockShowingArgs", {
    "showing": "Showing",
})
"""The arguments of `set_dock_showing`."""

SetDockSizeArgs = TypedDict("SetDockSizeArgs", {
    "size": "Size",
})
"""The arguments of `set_dock_size`."""

ShowDockArgs = TypedDict("ShowDockArgs", {
    "on": NotRequired["bool | None"],
})
"""The arguments of `show_dock`."""

SetDockRunningArgs = TypedDict("SetDockRunningArgs", {
    "on": "bool",
})
"""The arguments of `set_dock_running`."""

SetDockLauncherArgs = TypedDict("SetDockLauncherArgs", {
    "on": "bool",
})
"""The arguments of `set_dock_launcher`."""

SetDockZoomArgs = TypedDict("SetDockZoomArgs", {
    "on": "bool",
})
"""The arguments of `set_dock_zoom`."""

MovePartArgs = TypedDict("MovePartArgs", {
    "part": "BarPart",
    "before": "BarPart",
})
"""The arguments of `move_part`."""

PlacePartArgs = TypedDict("PlacePartArgs", {
    "part": "BarPart",
    "position": "int",
})
"""The arguments of `place_part`."""

ShowPartArgs = TypedDict("ShowPartArgs", {
    "part": "BarPart",
    "on": "bool",
})
"""The arguments of `show_part`."""

MoveItemArgs = TypedDict("MoveItemArgs", {
    "item": "DockItem",
    "before": "DockItem",
})
"""The arguments of `move_item`."""

PlaceItemArgs = TypedDict("PlaceItemArgs", {
    "item": "DockItem",
    "position": "int",
})
"""The arguments of `place_item`."""

PinArgs = TypedDict("PinArgs", {
    "key": "str",
    "title": "str",
    "icon": "str",
    "next": NotRequired["str | None"],
})
"""The arguments of `pin`."""

UnpinArgs = TypedDict("UnpinArgs", {
    "item": "DockItem",
})
"""The arguments of `unpin`."""

OpenStyleSettingsArgs = TypedDict("OpenStyleSettingsArgs", {
})
"""The arguments of `open_style_settings`."""

CloseStyleSettingsArgs = TypedDict("CloseStyleSettingsArgs", {
})
"""The arguments of `close_style_settings`."""

ShowStyleSettingsArgs = TypedDict("ShowStyleSettingsArgs", {
    "on": NotRequired["bool | None"],
})
"""The arguments of `show_style_settings`."""

SetStripsTabsArgs = TypedDict("SetStripsTabsArgs", {
    "on": "bool",
})
"""The arguments of `set_strips_tabs`."""

SetFramesSnapArgs = TypedDict("SetFramesSnapArgs", {
    "on": "bool",
})
"""The arguments of `set_frames_snap`."""

TabPlaceHereArgs = TypedDict("TabPlaceHereArgs", {
    "s": "LayoutStrip",
    "target": "Any",
})
"""The arguments of `tab_place_here`."""

ReorderPlaceArgs = TypedDict("ReorderPlaceArgs", {
    "s": "LayoutStrip",
    "place": "Any",
    "at": "int",
})
"""The arguments of `reorder_place`."""

BringForwardArgs = TypedDict("BringForwardArgs", {
    "s": "LayoutStrip",
})
"""The arguments of `bring_forward`."""

ShowStyleArgs = TypedDict("ShowStyleArgs", {
    "style": "str",
})
"""The arguments of `show_style`."""

SizePlaceArgs = TypedDict("SizePlaceArgs", {
    "key": "str",
    "size": "list[int]",
})
"""The arguments of `size_place`."""

PlaceDroppedArgs = TypedDict("PlaceDroppedArgs", {
    "target": "Any",
    "at": "list[int]",
})
"""The arguments of `place_dropped`."""

QueryShowArgs = TypedDict("QueryShowArgs", {
    "place": "str",
    "found": "Any",
})
"""The arguments of `query_show`."""

QueryRetryArgs = TypedDict("QueryRetryArgs", {
    "place": "str",
})
"""The arguments of `query_retry`."""

ShowPlaceArgs = TypedDict("ShowPlaceArgs", {
    "place": "Any",
})
"""The arguments of `show_place`."""

class Shell(Resource):
    """Shell — `tl.so.tilo.shell.Shell`."""
    __surface__ = "so.tilo.shell@344292e2209225c3"
    qsrjrjkd: ChromePreset  # preset
    @property
    def _2se66uzq(self) -> ChromePreset:
        """based_on (`2se66uzq` on the wire)."""
        return self.fields["2se66uzq"]

    @_2se66uzq.setter
    def _2se66uzq(self, value: ChromePreset) -> None:
        self.edit("2se66uzq", value=value)
    nt6y5bk3: bool  # tuned
    kqnswwzm: bool  # bar_shown
    mi3yni2m: Edge  # bar_edge
    la4yrobd: Fit  # bar_fit
    @property
    def _6ujrnpej(self) -> Finish:
        """bar_finish (`6ujrnpej` on the wire)."""
        return self.fields["6ujrnpej"]

    @_6ujrnpej.setter
    def _6ujrnpej(self, value: Finish) -> None:
        self.edit("6ujrnpej", value=value)
    b2fcmpwg: Showing  # bar_showing
    @property
    def _773tn644(self) -> Size:
        """bar_size (`773tn644` on the wire)."""
        return self.fields["773tn644"]

    @_773tn644.setter
    def _773tn644(self, value: Size) -> None:
        self.edit("773tn644", value=value)
    voqg5seo: list[Entry[BarPart]]  # bar_parts
    b7grsgnv: bool  # dock_shown
    o6tdqxq3: Edge  # dock_edge
    dq6kzv2v: Fit  # dock_fit
    y5rlj3v3: Finish  # dock_finish
    nz74n3iu: Showing  # dock_showing
    wx25jsef: Size  # dock_size
    hx5alotd: list[Entry[DockItem]]  # dock_items
    ffa2ohzm: bool  # dock_running
    h5oo7js2: bool  # dock_launcher
    @property
    def _5n4s55tz(self) -> bool:
        """dock_zoom (`5n4s55tz` on the wire)."""
        return self.fields["5n4s55tz"]

    @_5n4s55tz.setter
    def _5n4s55tz(self, value: bool) -> None:
        self.edit("5n4s55tz", value=value)
    @property
    def _4zknrelh(self) -> bool:
        """settings_open (`4zknrelh` on the wire)."""
        return self.fields["4zknrelh"]

    @_4zknrelh.setter
    def _4zknrelh(self, value: bool) -> None:
        self.edit("4zknrelh", value=value)
    dwct4abs: bool  # strips_tabs
    gfmzi3qr: bool  # frames_snap
    @property
    def _6bamt22e(self) -> bool:
        """canvas_asked (`6bamt22e` on the wire)."""
        return self.fields["6bamt22e"]

    @_6bamt22e.setter
    def _6bamt22e(self, value: bool) -> None:
        self.edit("6bamt22e", value=value)

    def asked_on_canvas(self) -> None:
        """The person asked on the canvas: the hint the empty canvas says until then has been said (`Shell.canvas_asked`), and stays away from now on."""
        return self.action("asked_on_canvas")

    def asked_on_canvas_many(self, items: list[AskedOnCanvasArgs]) -> list[dict[str, Any]]:
        """Runs `asked_on_canvas` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("asked_on_canvas", items)

    def apply(self, *, layout: ChromePreset) -> None:
        """Uses the preset `layout`: every setting it names at once. The result is exactly that preset, so the picker shows it chosen and 'custom' is off."""
        return self.action("apply", layout=layout)

    def apply_many(self, items: list[ApplyArgs]) -> list[dict[str, Any]]:
        """Runs `apply` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("apply", items)

    def show_app_menus(self) -> None:
        """Puts the focused app's menus on the bar, after the system's: what the app in front can do now, on its resource."""
        return self.action("show_app_menus")

    def show_app_menus_many(self, items: list[ShowAppMenusArgs]) -> list[dict[str, Any]]:
        """Runs `show_app_menus` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show_app_menus", items)

    def hide_app_menus(self) -> None:
        """Takes the focused app's menus off the bar."""
        return self.action("hide_app_menus")

    def hide_app_menus_many(self, items: list[HideAppMenusArgs]) -> list[dict[str, Any]]:
        """Runs `hide_app_menus` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("hide_app_menus", items)

    def reset(self) -> None:
        """Returns the shell to the preset it was based on, dropping the person's tuning."""
        return self.action("reset")

    def reset_many(self, items: list[ResetArgs]) -> list[dict[str, Any]]:
        """Runs `reset` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("reset", items)

    def bar_on(self, *, edge: Edge) -> None:
        """Puts the bar on `edge`."""
        return self.action("bar_on", edge=edge)

    def bar_on_many(self, items: list[BarOnArgs]) -> list[dict[str, Any]]:
        """Runs `bar_on` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("bar_on", items)

    def set_bar_fit(self, *, fit: Fit) -> None:
        """Runs the app's action."""
        return self.action("set_bar_fit", fit=fit)

    def set_bar_fit_many(self, items: list[SetBarFitArgs]) -> list[dict[str, Any]]:
        """Runs `set_bar_fit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_bar_fit", items)

    def set_bar_finish(self, *, finish: Finish) -> None:
        """Runs the app's action."""
        return self.action("set_bar_finish", finish=finish)

    def set_bar_finish_many(self, items: list[SetBarFinishArgs]) -> list[dict[str, Any]]:
        """Runs `set_bar_finish` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_bar_finish", items)

    def set_bar_showing(self, *, showing: Showing) -> None:
        """Runs the app's action."""
        return self.action("set_bar_showing", showing=showing)

    def set_bar_showing_many(self, items: list[SetBarShowingArgs]) -> list[dict[str, Any]]:
        """Runs `set_bar_showing` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_bar_showing", items)

    def set_bar_size(self, *, size: Size) -> None:
        """Runs the app's action."""
        return self.action("set_bar_size", size=size)

    def set_bar_size_many(self, items: list[SetBarSizeArgs]) -> list[dict[str, Any]]:
        """Runs `set_bar_size` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_bar_size", items)

    def show_bar(self,
        *,
        on: bool | None = None,
    ) -> None:
        """Shows or hides the top bar: the one control that turns it on and off, so a person sees whether it is on where they press it."""
        args: dict[str, Any] = {}
        if on is not None:
            args["on"] = on
        return self.action("show_bar", **args)

    def show_bar_many(self, items: list[ShowBarArgs]) -> list[dict[str, Any]]:
        """Runs `show_bar` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show_bar", items)

    def dock_on(self, *, edge: Edge) -> None:
        """Moves the dock to `edge`, keeping how it is built."""
        return self.action("dock_on", edge=edge)

    def dock_on_many(self, items: list[DockOnArgs]) -> list[dict[str, Any]]:
        """Runs `dock_on` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("dock_on", items)

    def set_dock_fit(self, *, fit: Fit) -> None:
        """Runs the app's action."""
        return self.action("set_dock_fit", fit=fit)

    def set_dock_fit_many(self, items: list[SetDockFitArgs]) -> list[dict[str, Any]]:
        """Runs `set_dock_fit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_dock_fit", items)

    def set_dock_finish(self, *, finish: Finish) -> None:
        """Runs the app's action."""
        return self.action("set_dock_finish", finish=finish)

    def set_dock_finish_many(self, items: list[SetDockFinishArgs]) -> list[dict[str, Any]]:
        """Runs `set_dock_finish` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_dock_finish", items)

    def set_dock_showing(self, *, showing: Showing) -> None:
        """Runs the app's action."""
        return self.action("set_dock_showing", showing=showing)

    def set_dock_showing_many(self, items: list[SetDockShowingArgs]) -> list[dict[str, Any]]:
        """Runs `set_dock_showing` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_dock_showing", items)

    def set_dock_size(self, *, size: Size) -> None:
        """Runs the app's action."""
        return self.action("set_dock_size", size=size)

    def set_dock_size_many(self, items: list[SetDockSizeArgs]) -> list[dict[str, Any]]:
        """Runs `set_dock_size` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_dock_size", items)

    def show_dock(self,
        *,
        on: bool | None = None,
    ) -> None:
        """Shows or hides the dock: the one control that turns it on and off, so a person sees whether it is on where they press it."""
        args: dict[str, Any] = {}
        if on is not None:
            args["on"] = on
        return self.action("show_dock", **args)

    def show_dock_many(self, items: list[ShowDockArgs]) -> list[dict[str, Any]]:
        """Runs `show_dock` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show_dock", items)

    def set_dock_running(self, *, on: bool) -> None:
        """Whether the dock shows the apps running that nobody pinned."""
        return self.action("set_dock_running", on=on)

    def set_dock_running_many(self, items: list[SetDockRunningArgs]) -> list[dict[str, Any]]:
        """Runs `set_dock_running` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_dock_running", items)

    def set_dock_launcher(self, *, on: bool) -> None:
        """Whether the dock has a launcher entry."""
        return self.action("set_dock_launcher", on=on)

    def set_dock_launcher_many(self, items: list[SetDockLauncherArgs]) -> list[dict[str, Any]]:
        """Runs `set_dock_launcher` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_dock_launcher", items)

    def set_dock_zoom(self, *, on: bool) -> None:
        """Whether the dock magnifies the item under the pointer."""
        return self.action("set_dock_zoom", on=on)

    def set_dock_zoom_many(self, items: list[SetDockZoomArgs]) -> list[dict[str, Any]]:
        """Runs `set_dock_zoom` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_dock_zoom", items)

    def move_part(self,
        *,
        part: BarPart,
        before: BarPart,
    ) -> None:
        """Puts a bar part with the one it comes before, wherever the part stands now: the one action a drag commits, and the same change from the settings, a shortcut or an agent."""
        return self.action("move_part", part=part, before=before)

    def move_part_many(self, items: list[MovePartArgs]) -> list[dict[str, Any]]:
        """Runs `move_part` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_part", items)

    def place_part(self,
        *,
        part: BarPart,
        position: int,
    ) -> None:
        """Puts a bar part at `position`, counted from the leading edge: what a drag that names a place gives."""
        return self.action("place_part", part=part, position=position)

    def place_part_many(self, items: list[PlacePartArgs]) -> list[dict[str, Any]]:
        """Runs `place_part` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("place_part", items)

    def show_part(self,
        *,
        part: BarPart,
        on: bool,
    ) -> None:
        """Shows or hides one part of the bar."""
        return self.action("show_part", part=part, on=on)

    def show_part_many(self, items: list[ShowPartArgs]) -> list[dict[str, Any]]:
        """Runs `show_part` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show_part", items)

    def move_item(self,
        *,
        item: DockItem,
        before: DockItem,
    ) -> None:
        """Puts a dock item with the one it comes before: the one action a drag commits."""
        return self.action("move_item", item=item, before=before)

    def move_item_many(self, items: list[MoveItemArgs]) -> list[dict[str, Any]]:
        """Runs `move_item` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_item", items)

    def place_item(self,
        *,
        item: DockItem,
        position: int,
    ) -> None:
        """Puts a dock item at `position`, counted from the leading edge: what a drag that names a place gives. An item the person's document does not hold yet — an app running on the dock that they never pinned, which the dock drew from the shell — is pinned where it lands, so moving an app they already run keeps it on the dock after it closes."""
        return self.action("place_item", item=item, position=position)

    def place_item_many(self, items: list[PlaceItemArgs]) -> list[dict[str, Any]]:
        """Runs `place_item` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("place_item", items)

    def pin(self,
        *,
        key: str,
        title: str,
        icon: str,
        next: str | None = None,
    ) -> None:
        """Pins a resource or an app to the dock, before `next` (or at the end when no neighbour is named): what dragging one onto the dock does, and what the person does from a menu."""
        args: dict[str, Any] = {}
        args["key"] = key
        args["title"] = title
        args["icon"] = icon
        if next is not None:
            args["next"] = next
        return self.action("pin", **args)

    def pin_many(self, items: list[PinArgs]) -> list[dict[str, Any]]:
        """Runs `pin` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("pin", items)

    def unpin(self, *, item: DockItem) -> None:
        """Takes an item off the dock."""
        return self.action("unpin", item=item)

    def unpin_many(self, items: list[UnpinArgs]) -> list[dict[str, Any]]:
        """Runs `unpin` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("unpin", items)

    def open_item(self, *, key: str) -> None:
        """What a press on a dock item does, by the app the item stands for: a window of that app on this screen comes to the front, so the shell says which window the person is in and the top bar's commands are its own; the window is the one the item was drawn for, so a press never opens a second copy. An app with no window open is opened (`session.open_app`), and the launcher shows. It takes the app's key and not the item: an item the shell's document holds reaches an action as a document reference, and the press names the one thing it needs, so a run's item reaches it without a `DockItem` parameter to fit."""
        return self.action("open_item", key=key)

    def open_style_settings(self) -> None:
        """The style's own settings, and the two ways of changing a style (`docs/designs/app-language/ shell.md` §6): without an agent, in the one place a person's settings live, and with an agent, by the same actions the person's controls run. Every setting here is plain tilolang state with a default (`chrome.tl`'s `Shell` document), so a person who never touches it gets the style as it ships, and the values live in the layout's own document, synced with the person's devices, with history and undo. Each setting is one action, so an agent — the canvas conversation or the REPL — changes it with the same call a press does, attributed and undoable: `shell.set_dock_edge(shell.shell, edge: shell.Edge.left)`. The style's settings stand over the shell (`style_panel`), opened by the bar's style menu (`style_menu`, `strips.tl`): a person changes the shell they look at without leaving it, as `shell.md` §6 asks. Opens the chosen style's settings over the shell: what the bar's style menu's entry runs, and what the REPL or the canvas agent calls too."""
        return self.action("open_style_settings")

    def open_style_settings_many(self, items: list[OpenStyleSettingsArgs]) -> list[dict[str, Any]]:
        """Runs `open_style_settings` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("open_style_settings", items)

    def close_style_settings(self) -> None:
        """Closes them."""
        return self.action("close_style_settings")

    def close_style_settings_many(self, items: list[CloseStyleSettingsArgs]) -> list[dict[str, Any]]:
        """Runs `close_style_settings` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("close_style_settings", items)

    def show_style_settings(self,
        *,
        on: bool | None = None,
    ) -> None:
        """Shows or hides their being open: one control for both, so a person sees the state where they press it."""
        args: dict[str, Any] = {}
        if on is not None:
            args["on"] = on
        return self.action("show_style_settings", **args)

    def show_style_settings_many(self, items: list[ShowStyleSettingsArgs]) -> list[dict[str, Any]]:
        """Runs `show_style_settings` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show_style_settings", items)

    def set_strips_tabs(self, *, on: bool) -> None:
        """Whether the strip of several places shows its tab bar: the strips style's own setting."""
        return self.action("set_strips_tabs", on=on)

    def set_strips_tabs_many(self, items: list[SetStripsTabsArgs]) -> list[dict[str, Any]]:
        """Runs `set_strips_tabs` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_strips_tabs", items)

    def set_frames_snap(self, *, on: bool) -> None:
        """Whether a place let go on the framed canvas lands on the grid: the frames style's own setting."""
        return self.action("set_frames_snap", on=on)

    def set_frames_snap_many(self, items: list[SetFramesSnapArgs]) -> list[dict[str, Any]]:
        """Runs `set_frames_snap` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_frames_snap", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"asked_on_canvas", "asked_on_canvas_many", "apply", "apply_many", "show_app_menus", "show_app_menus_many", "hide_app_menus", "hide_app_menus_many", "reset", "reset_many", "bar_on", "bar_on_many", "set_bar_fit", "set_bar_fit_many", "set_bar_finish", "set_bar_finish_many", "set_bar_showing", "set_bar_showing_many", "set_bar_size", "set_bar_size_many", "show_bar", "show_bar_many", "dock_on", "dock_on_many", "set_dock_fit", "set_dock_fit_many", "set_dock_finish", "set_dock_finish_many", "set_dock_showing", "set_dock_showing_many", "set_dock_size", "set_dock_size_many", "show_dock", "show_dock_many", "set_dock_running", "set_dock_running_many", "set_dock_launcher", "set_dock_launcher_many", "set_dock_zoom", "set_dock_zoom_many", "move_part", "move_part_many", "place_part", "place_part_many", "show_part", "show_part_many", "move_item", "move_item_many", "place_item", "place_item_many", "pin", "pin_many", "unpin", "unpin_many", "open_item", "open_style_settings", "open_style_settings_many", "close_style_settings", "close_style_settings_many", "show_style_settings", "show_style_settings_many", "set_strips_tabs", "set_strips_tabs_many", "set_frames_snap", "set_frames_snap_many"})


class BarPart(Resource):
    """BarPart — `tl.so.tilo.shell.BarPart`."""
    __surface__ = "so.tilo.shell@344292e2209225c3"
    fjzbw5un: BarPartKind  # kind
    pbly7ymy: bool  # shown


class DockItem(Resource):
    """DockItem — `tl.so.tilo.shell.DockItem`."""
    __surface__ = "so.tilo.shell@344292e2209225c3"
    rfsefuzv: str  # key
    qsnrl2gk: str  # title
    @property
    def _6gyb7csc(self) -> str:
        """icon (`6gyb7csc` on the wire)."""
        return self.fields["6gyb7csc"]

    @_6gyb7csc.setter
    def _6gyb7csc(self, value: str) -> None:
        self.edit("6gyb7csc", value=value)
    k376lylv: bool  # pinned
    @property
    def _5vpgvzqb(self) -> bool:
        """running (`5vpgvzqb` on the wire)."""
        return self.fields["5vpgvzqb"]

    @_5vpgvzqb.setter
    def _5vpgvzqb(self, value: bool) -> None:
        self.edit("5vpgvzqb", value=value)
    mgcmvxjx: int  # badge
    e7ft4mly: bool  # attention


class Layout(Resource):
    """Layout — `tl.so.tilo.shell.Layout`."""
    __surface__ = "so.tilo.shell@344292e2209225c3"
    uykuo63i: str  # person
    k4p3z4ee: str  # front
    yb7sokzs: list[Entry[LayoutPerson]]  # people
    @property
    def _7n7jrcg3(self) -> list[Entry[LayoutWorkspace]]:
        """workspaces (`7n7jrcg3` on the wire)."""
        return self.fields["7n7jrcg3"]

    @_7n7jrcg3.setter
    def _7n7jrcg3(self, value: list[Entry[LayoutWorkspace]]) -> None:
        self.edit("7n7jrcg3", value=value)
    hqul2vwj: list[Entry[LayoutRow]]  # rows
    @property
    def _7lq6xkzz(self) -> list[Entry[LayoutStrip]]:
        """strips (`7lq6xkzz` on the wire)."""
        return self.fields["7lq6xkzz"]

    @_7lq6xkzz.setter
    def _7lq6xkzz(self, value: list[Entry[LayoutStrip]]) -> None:
        self.edit("7lq6xkzz", value=value)
    @property
    def _2ug5q5cq(self) -> list[Entry[LayoutPlace]]:
        """places (`2ug5q5cq` on the wire)."""
        return self.fields["2ug5q5cq"]

    @_2ug5q5cq.setter
    def _2ug5q5cq(self, value: list[Entry[LayoutPlace]]) -> None:
        self.edit("2ug5q5cq", value=value)
    dt4vnrqa: list[Entry[LayoutQuery]]  # queries
    fqirvfzw: list[Entry[LayoutRef]]  # refs

    def a_workspace(self, *, name: str) -> str:
        """Adds a workspace, with one row, to `a`, and returns the row's key."""
        return self.action("a_workspace", name=name)

    def a_front_workspace(self, *, name: str) -> str:
        """Adds the workspace in front, with its row in front, to `a`, and returns the row's key."""
        return self.action("a_front_workspace", name=name)

    def a_strip(self,
        *,
        row: str,
        name: str,
        width: int,
    ) -> str:
        """Adds a strip named `name` to `row` at `width`, and returns its key."""
        return self.action("a_strip", row=row, name=name, width=width)

    def a_note_place(self,
        *,
        strip: str,
        index: int,
    ) -> str:
        """Adds a place showing the note at `index` of the notebook to `strip`, and returns its key."""
        return self.action("a_note_place", strip=strip, index=index)

    def a_foreign_place(self,
        *,
        strip: str,
        index: int,
        title: str,
    ) -> str:
        """A place of a kind this build has no document for: it stands as it is, which is what a place another shell made looks like."""
        return self.action("a_foreign_place", strip=strip, index=index, title=title)

    def a_query_place(self,
        *,
        strip: str,
        text: str,
    ) -> str:
        """Adds a place showing a search for `text` to `strip`, with the two matches the notes app answered, and returns the place's key: the state a place stands in until the person chooses one of the resources it found (shell.md §8)."""
        return self.action("a_query_place", strip=strip, text=text)

    def tab_place_here(self,
        *,
        s: LayoutStrip,
        target: Any,
    ) -> None:
        """Tab the place whose window's bar was dragged onto the strip `s` into that strip: a person drags one window's bar onto another's to make its tabs, and the window's menu's 'Move into … as a tab' runs the same arranging action. The place lands after the strip's other tabs and comes to the front of them, so the window shows what was dropped on it. A place already in this strip is only brought to the front, so dropping a window on itself raises it rather than opening a second place. The strip comes forward with it."""
        return self.action("tab_place_here", s=s, target=target)

    def tab_place_here_many(self, items: list[TabPlaceHereArgs]) -> list[dict[str, Any]]:
        """Runs `tab_place_here` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("tab_place_here", items)

    def reorder_place(self,
        *,
        s: LayoutStrip,
        place: Any,
        at: int,
    ) -> None:
        """Rank the place `place` at `at` among its strip's tabs: what dragging a pill within the window's bar does, as the shell's own `move` with the place it goes before. The strip reports the pill's place among the tabs with the moved pill taken out, so the tab it lands before is that index of the others; the last place goes last."""
        return self.action("reorder_place", s=s, place=place, at=at)

    def reorder_place_many(self, items: list[ReorderPlaceArgs]) -> list[dict[str, Any]]:
        """Runs `reorder_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("reorder_place", items)

    def bring_forward(self, *, s: LayoutStrip) -> None:
        """Brings the window of the strip `s` to the front, the keyboard in it: the session brings its place in front forward, as it does for a press, a script or an agent alike. A strip with no place has no window to bring."""
        return self.action("bring_forward", s=s)

    def bring_forward_many(self, items: list[BringForwardArgs]) -> list[dict[str, Any]]:
        """Runs `bring_forward` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("bring_forward", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"a_workspace", "a_front_workspace", "a_strip", "a_note_place", "a_foreign_place", "a_query_place", "tab_place_here", "tab_place_here_many", "reorder_place", "reorder_place_many", "bring_forward", "bring_forward_many"})


class LayoutQuery(Resource):
    """LayoutQuery — `tl.so.tilo.shell.LayoutQuery`."""
    __surface__ = "so.tilo.shell@344292e2209225c3"
    @property
    def _6jget5cg(self) -> str:
        """key (`6jget5cg` on the wire)."""
        return self.fields["6jget5cg"]

    @_6jget5cg.setter
    def _6jget5cg(self, value: str) -> None:
        self.edit("6jget5cg", value=value)
    iwuvfj66: str  # text
    ybpst64r: str  # in_app
    ibasoiis: str  # kind
    a6ewjjom: str  # state
    xsjcdedm: str  # reason


class LayoutRef(Resource):
    """LayoutRef — `tl.so.tilo.shell.LayoutRef`."""
    __surface__ = "so.tilo.shell@344292e2209225c3"
    k652vouu: str  # key
    vdsinb5l: str  # query
    vj5mgxi6: str  # address
    pnom4n55: str  # title
    @property
    def _7giiatvv(self) -> str:
        """kind (`7giiatvv` on the wire)."""
        return self.fields["7giiatvv"]

    @_7giiatvv.setter
    def _7giiatvv(self, value: str) -> None:
        self.edit("7giiatvv", value=value)


class LayoutWorkspace(Resource):
    """LayoutWorkspace — `tl.so.tilo.shell.LayoutWorkspace`."""
    __surface__ = "so.tilo.shell@344292e2209225c3"
    @property
    def _5zibpqs6(self) -> str:
        """key (`5zibpqs6` on the wire)."""
        return self.fields["5zibpqs6"]

    @_5zibpqs6.setter
    def _5zibpqs6(self, value: str) -> None:
        self.edit("5zibpqs6", value=value)
    b2vzzczj: str  # name
    hwfwvs3f: bool  # current


class LayoutPerson(Resource):
    """LayoutPerson — `tl.so.tilo.shell.LayoutPerson`."""
    __surface__ = "so.tilo.shell@344292e2209225c3"
    @property
    def _3rpmhtjf(self) -> str:
        """key (`3rpmhtjf` on the wire)."""
        return self.fields["3rpmhtjf"]

    @_3rpmhtjf.setter
    def _3rpmhtjf(self, value: str) -> None:
        self.edit("3rpmhtjf", value=value)
    @property
    def _6a7urcxy(self) -> str:
        """name (`6a7urcxy` on the wire)."""
        return self.fields["6a7urcxy"]

    @_6a7urcxy.setter
    def _6a7urcxy(self, value: str) -> None:
        self.edit("6a7urcxy", value=value)


class LayoutRow(Resource):
    """LayoutRow — `tl.so.tilo.shell.LayoutRow`."""
    __surface__ = "so.tilo.shell@344292e2209225c3"
    @property
    def _3xc5xbon(self) -> str:
        """key (`3xc5xbon` on the wire)."""
        return self.fields["3xc5xbon"]

    @_3xc5xbon.setter
    def _3xc5xbon(self, value: str) -> None:
        self.edit("3xc5xbon", value=value)
    v5ef7tkv: str  # workspace
    iqgu3ggi: str  # name
    sm4scdu2: bool  # current
    bowkspu3: str  # shell


class LayoutStrip(Resource):
    """LayoutStrip — `tl.so.tilo.shell.LayoutStrip`."""
    __surface__ = "so.tilo.shell@344292e2209225c3"
    dufcke3u: str  # key
    x42tiiqe: str  # workspace
    dvberkpa: str  # row
    @property
    def _7ekq6qgi(self) -> int:
        """width (`7ekq6qgi` on the wire)."""
        return self.fields["7ekq6qgi"]

    @_7ekq6qgi.setter
    def _7ekq6qgi(self, value: int) -> None:
        self.edit("7ekq6qgi", value=value)
    @property
    def _2cbikm5m(self) -> bool:
        """maximized (`2cbikm5m` on the wire)."""
        return self.fields["2cbikm5m"]

    @_2cbikm5m.setter
    def _2cbikm5m(self, value: bool) -> None:
        self.edit("2cbikm5m", value=value)
    yk2oxjrj: str  # active
    @property
    def _44lq6lon(self) -> bool:
        """placed (`44lq6lon` on the wire)."""
        return self.fields["44lq6lon"]

    @_44lq6lon.setter
    def _44lq6lon(self, value: bool) -> None:
        self.edit("44lq6lon", value=value)
    vbv2zrob: int  # canvas_x
    jvrwsckz: int  # canvas_y
    m75kjoue: int  # canvas_width
    s3lpes2e: int  # canvas_height
    s6rivl3a: bool  # canvas_hugs
    pznsxbgq: str  # shell


class LayoutPlace(Resource):
    """LayoutPlace — `tl.so.tilo.shell.LayoutPlace`."""
    __surface__ = "so.tilo.shell@344292e2209225c3"
    ba3kyokq: str  # key
    @property
    def _7dno3n47(self) -> str:
        """strip (`7dno3n47` on the wire)."""
        return self.fields["7dno3n47"]

    @_7dno3n47.setter
    def _7dno3n47(self, value: str) -> None:
        self.edit("7dno3n47", value=value)
    jpzk4cli: str  # shows
    nprpusuo: str  # shown_as
    ofkshr4v: str  # resource
    qqcejiim: str  # title
    jeatudax: str  # name
    vkbhstef: bool  # pinned
    soy4vimv: str  # shell


class Viewing(Resource):
    """Viewing — `tl.so.tilo.shell.Viewing`."""
    __surface__ = "so.tilo.shell@344292e2209225c3"
    kna5sco4: str  # style

    def show_style(self, *, style: str) -> None:
        """Shows the shell through the style `style`: a package and a view (`so.tilo.shell:strips`). Switching keeps the layout: the same places stand where they were, drawn another way."""
        return self.action("show_style", style=style)

    def show_style_many(self, items: list[ShowStyleArgs]) -> list[dict[str, Any]]:
        """Runs `show_style` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show_style", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"show_style", "show_style_many"})


class TiloShell(Resource):
    """Shell — `tl.so.tilo.shell`."""
    __surface__ = "so.tilo.shell@344292e2209225c3"
    def size_place(self,
        *,
        key: str,
        size: list[int],
    ) -> None:
        """Gives the place `key` the size a drag of its edge or corner left it at, `size` its width and height in canvas pixels: one arranging step of the person's, its corner where it stood."""
        return self.action("size_place", key=key, size=size)

    def size_place_many(self, items: list[SizePlaceArgs]) -> list[dict[str, Any]]:
        """Runs `size_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("size_place", items)

    def drew_place(self,
        *,
        key: str,
        size: list[int],
    ) -> None:
        """Keeps how tall the hugging place `key` is drawn now, `size` carrying its height in canvas pixels as the map lays it out: the free area under it on the canvas is what the person sees there (canvas.md §1, §7), though the place could grow into it. View state the person's own canvas reports, never a step of arranging: Undo leaves it alone."""
        return self.action("drew_place", key=key, size=size)

    def place_dropped(self,
        *,
        target: Any,
        at: list[int],
    ) -> None:
        """Puts a resource dropped on the canvas where it was let go. A place that already stands on the canvas — the place whose handle was dragged — moves to the point it was let go at, through its own `position` action, so a drag that ends on the canvas moves the place and never stands a second one. Any other resource — a message's attachment, a search hit, a row of another view — is placed at that point through the system's own `place`, which is what an agent's `place_at` runs. Either is one arranging step of the person's, attributed and undoable."""
        return self.action("place_dropped", target=target, at=at)

    def place_dropped_many(self, items: list[PlaceDroppedArgs]) -> list[dict[str, Any]]:
        """Runs `place_dropped` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("place_dropped", items)

    def open_first(self, *, query: str) -> None:
        """Enter's work on a query: the first app the text finds opens, so typing 'no' and pressing Enter opens Notes; a query that names no app is what search everywhere answers, as Enter has always done for what the shell's own rows do not hold."""
        return self.action("open_first", query=query)

    def toggle_vpn(self, *, v: Any) -> None:
        """Turn the VPN on through its first profile, or off through the one connected: the tile's one press, whichever way it flips."""
        return self.action("toggle_vpn", v=v)

    def a_style_settings(self, *, style: str) -> Layout:
        """A shell of one note in the style named, with that place in front, and the style's own settings open over it: what a person sees who opens them from the bar's style menu."""
        return self.action("a_style_settings", style=style)

    def query_show(self,
        *,
        place: str,
        found: Any,
    ) -> None:
        """Show `found` in the place `place` while it shows a query: the same place, now the found resource (shell.md §8)."""
        return self.action("query_show", place=place, found=found)

    def query_show_many(self, items: list[QueryShowArgs]) -> list[dict[str, Any]]:
        """Runs `query_show` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("query_show", items)

    def query_retry(self, *, place: str) -> None:
        """Look for what the place `place`'s search names again."""
        return self.action("query_retry", place=place)

    def query_retry_many(self, items: list[QueryRetryArgs]) -> list[dict[str, Any]]:
        """Runs `query_retry` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("query_retry", items)

    def detach_place(self, *, place: Any) -> None:
        """Give the place `place` a window of its own: what dragging a tab out of its window's bar does, as the shell's own `split-right` for the window in front. The place stands in front of its new window, and that window comes to the front of the shell."""
        return self.action("detach_place", place=place)

    def show_place(self, *, place: Any) -> None:
        """Shows the place `place` and brings its window forward with it, the keyboard in it: what choosing a tab does, and the same arranging action a script or an agent runs."""
        return self.action("show_place", place=place)

    def show_place_many(self, items: list[ShowPlaceArgs]) -> list[dict[str, Any]]:
        """Runs `show_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show_place", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"size_place", "size_place_many", "drew_place", "place_dropped", "place_dropped_many", "open_first", "toggle_vpn", "a_style_settings", "query_show", "query_show_many", "query_retry", "query_retry_many", "detach_place", "show_place", "show_place_many"})


