"""Types for Browser (`so.tilo.browser`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Bookmark` is the app's `tl.so.tilo.browser.Bookmark` at `browser/<resource>`, read with
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
    "Bookmark",
    "Visit",
    "ImportedVisit",
    "RememberedForm",
    "Question",
    "Address",
    "Page",
    "PageStep",
    "DialogAsk",
    "MenuAsk",
    "MenuRow",
    "Download",
    "SiteInfo",
    "HistoryEntry",
    "Extension",
    "LoginForm",
    "LoginAccount",
    "Ask",
    "Answer",
    "Browser",
    "TiloBrowser",
    "RemoveBookmarkArgs",
    "NameBookmarkArgs",
    "MoveBookmarkArgs",
    "NameVisitArgs",
    "ForgetVisitArgs",
    "ForgetFormArgs",
    "DeleteAddressArgs",
    "UpdateDownloadArgs",
    "OpenFromPageArgs",
    "RememberHistoryArgs",
    "RememberExtensionsArgs",
    "NoteExtensionArgs",
    "RememberCredentialArgs",
    "WentToArgs",
    "TogglePauseArgs",
    "BookmarkArgs",
    "SetSiteProtectionArgs",
    "ToggleSiteProtectionArgs",
    "ForgetSiteFormsArgs",
    "ForgetAllFormsArgs",
    "RememberFormArgs",
    "ClearHistoryArgs",
    "VisitArgs",
    "TrimHistoryArgs",
    "ArrivedAtArgs",
    "ImportBrowsingArgs",
    "ImportHistoryArgs",
    "ImportBookmarksArgs",
    "RevealDownloadArgs",
]

RemoveBookmarkArgs = TypedDict("RemoveBookmarkArgs", {
})
"""The arguments of `remove_bookmark`."""

NameBookmarkArgs = TypedDict("NameBookmarkArgs", {
    "title": "str",
})
"""The arguments of `name_bookmark`."""

MoveBookmarkArgs = TypedDict("MoveBookmarkArgs", {
    "folder": "str",
})
"""The arguments of `move_bookmark`."""

NameVisitArgs = TypedDict("NameVisitArgs", {
    "title": "str",
})
"""The arguments of `name_visit`."""

ForgetVisitArgs = TypedDict("ForgetVisitArgs", {
})
"""The arguments of `forget_visit`."""

ForgetFormArgs = TypedDict("ForgetFormArgs", {
})
"""The arguments of `forget_form`."""

DeleteAddressArgs = TypedDict("DeleteAddressArgs", {
})
"""The arguments of `delete_address`."""

UpdateDownloadArgs = TypedDict("UpdateDownloadArgs", {
    "id": "int",
    "state": "str",
    "name": "str",
    "url": "str",
    "path": "str",
    "retryable": "bool",
    "received": "int",
    "total": "int",
    "error": "str",
})
"""The arguments of `update_download`."""

OpenFromPageArgs = TypedDict("OpenFromPageArgs", {
    "url": "str",
    "popup": "bool",
})
"""The arguments of `open_from_page`."""

RememberHistoryArgs = TypedDict("RememberHistoryArgs", {
    "direction": "str",
    "entries": "str",
})
"""The arguments of `remember_history`."""

RememberExtensionsArgs = TypedDict("RememberExtensionsArgs", {
    "installed": "str",
    "message": "str",
})
"""The arguments of `remember_extensions`."""

NoteExtensionArgs = TypedDict("NoteExtensionArgs", {
    "ok": "bool",
    "kind": "str",
    "message": "str",
    "id": "str",
})
"""The arguments of `note_extension`."""

RememberCredentialArgs = TypedDict("RememberCredentialArgs", {
    "did": "str",
    "success": "bool",
    "message": "str",
    "forms": "str",
    "accounts": "str",
    "capture": "str",
    "username": "str",
    "late": "bool",
})
"""The arguments of `remember_credential`."""

WentToArgs = TypedDict("WentToArgs", {
    "url": "str",
})
"""The arguments of `went_to`."""

TogglePauseArgs = TypedDict("TogglePauseArgs", {
    "paused": NotRequired["bool | None"],
})
"""The arguments of `toggle_pause`."""

BookmarkArgs = TypedDict("BookmarkArgs", {
    "url": "str",
    "title": NotRequired["str | None"],
})
"""The arguments of `bookmark`."""

SetSiteProtectionArgs = TypedDict("SetSiteProtectionArgs", {
    "origin": "str",
    "enabled": "bool",
})
"""The arguments of `set_site_protection`."""

ToggleSiteProtectionArgs = TypedDict("ToggleSiteProtectionArgs", {
    "origin": "str",
})
"""The arguments of `toggle_site_protection`."""

ForgetSiteFormsArgs = TypedDict("ForgetSiteFormsArgs", {
    "origin": "str",
})
"""The arguments of `forget_site_forms`."""

ForgetAllFormsArgs = TypedDict("ForgetAllFormsArgs", {
})
"""The arguments of `forget_all_forms`."""

RememberFormArgs = TypedDict("RememberFormArgs", {
    "origin": "str",
    "signature": "str",
    "label": "str",
    "page": "str",
    "questions": "list[str]",
    "whole": "bool",
})
"""The arguments of `remember_form`."""

ClearHistoryArgs = TypedDict("ClearHistoryArgs", {
})
"""The arguments of `clear_history`."""

VisitArgs = TypedDict("VisitArgs", {
    "url": "str",
    "title": "str",
    "at": "str",
})
"""The arguments of `visit`."""

TrimHistoryArgs = TypedDict("TrimHistoryArgs", {
    "most": "int",
})
"""The arguments of `trim_history`."""

ArrivedAtArgs = TypedDict("ArrivedAtArgs", {
    "url": "str",
})
"""The arguments of `arrived_at`."""

ImportBrowsingArgs = TypedDict("ImportBrowsingArgs", {
    "file": "Any",
})
"""The arguments of `import_browsing`."""

ImportHistoryArgs = TypedDict("ImportHistoryArgs", {
    "bundle": "str",
})
"""The arguments of `import_history`."""

ImportBookmarksArgs = TypedDict("ImportBookmarksArgs", {
    "bundle": "str",
})
"""The arguments of `import_bookmarks`."""

RevealDownloadArgs = TypedDict("RevealDownloadArgs", {
    "path": "str",
})
"""The arguments of `reveal_download`."""

class Bookmark(Resource):
    """Bookmark — `tl.so.tilo.browser.Bookmark`."""
    __surface__ = "so.tilo.browser@172587f7789744b0"
    @property
    def _44buv5tt(self) -> str:
        """url (`44buv5tt` on the wire)."""
        return self.fields["44buv5tt"]

    @_44buv5tt.setter
    def _44buv5tt(self, value: str) -> None:
        self.edit("44buv5tt", value=value)
    b3zhsydm: str  # title
    ehmpzm3r: str  # folder

    def remove_bookmark(self) -> None:
        """Takes a bookmark back."""
        return self.action("remove_bookmark")

    def remove_bookmark_many(self, items: list[RemoveBookmarkArgs]) -> list[dict[str, Any]]:
        """Runs `remove_bookmark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_bookmark", items)

    def name_bookmark(self, *, title: str) -> None:
        """Renames a saved page, when the person likes their own name for it better than its title."""
        return self.action("name_bookmark", title=title)

    def name_bookmark_many(self, items: list[NameBookmarkArgs]) -> list[dict[str, Any]]:
        """Runs `name_bookmark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("name_bookmark", items)

    def move_bookmark(self, *, folder: str) -> None:
        """Puts a bookmark in a folder of the person's library, such as `Work/Reading`, or at the top of the bookmarks when the folder is empty."""
        return self.action("move_bookmark", folder=folder)

    def move_bookmark_many(self, items: list[MoveBookmarkArgs]) -> list[dict[str, Any]]:
        """Runs `move_bookmark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_bookmark", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"remove_bookmark", "remove_bookmark_many", "name_bookmark", "name_bookmark_many", "move_bookmark", "move_bookmark_many"})


class Visit(Resource):
    """Visit — `tl.so.tilo.browser.Visit`."""
    __surface__ = "so.tilo.browser@172587f7789744b0"
    k73mxvmp: str  # url
    qy6vtgjc: str  # title
    @property
    def _2yoxl6hx(self) -> str:
        """last (`2yoxl6hx` on the wire)."""
        return self.fields["2yoxl6hx"]

    @_2yoxl6hx.setter
    def _2yoxl6hx(self, value: str) -> None:
        self.edit("2yoxl6hx", value=value)
    eruwucur: int  # count

    def name_visit(self, *, title: str) -> None:
        """Names a visit, when the person likes their own name for a page better than its title."""
        return self.action("name_visit", title=title)

    def name_visit_many(self, items: list[NameVisitArgs]) -> list[dict[str, Any]]:
        """Runs `name_visit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("name_visit", items)

    def forget_visit(self) -> None:
        """Forgets one visit. A bookmark of the page stays."""
        return self.action("forget_visit")

    def forget_visit_many(self, items: list[ForgetVisitArgs]) -> list[dict[str, Any]]:
        """Runs `forget_visit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("forget_visit", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"name_visit", "name_visit_many", "forget_visit", "forget_visit_many"})


class ImportedVisit(Resource):
    """ImportedVisit — `tl.so.tilo.browser.ImportedVisit`."""
    __surface__ = "so.tilo.browser@172587f7789744b0"
    my4dhx2s: str  # url
    @property
    def _43vnrbh6(self) -> str:
        """title (`43vnrbh6` on the wire)."""
        return self.fields["43vnrbh6"]

    @_43vnrbh6.setter
    def _43vnrbh6(self, value: str) -> None:
        self.edit("43vnrbh6", value=value)
    avuwnnrf: str  # last
    srpuiw6c: int  # count


class RememberedForm(Resource):
    """RememberedForm — `tl.so.tilo.browser.RememberedForm`."""
    __surface__ = "so.tilo.browser@172587f7789744b0"
    @property
    def _3wdvc5zt(self) -> str:
        """origin (`3wdvc5zt` on the wire)."""
        return self.fields["3wdvc5zt"]

    @_3wdvc5zt.setter
    def _3wdvc5zt(self, value: str) -> None:
        self.edit("3wdvc5zt", value=value)
    ksw3jnev: str  # signature
    vdjubgjc: str  # label
    @property
    def _6atmgq6f(self) -> list[str]:
        """pages (`6atmgq6f` on the wire)."""
        return self.fields["6atmgq6f"]

    @_6atmgq6f.setter
    def _6atmgq6f(self, value: list[str]) -> None:
        self.edit("6atmgq6f", value=value)
    znuzitns: int  # seen
    kexnvpi2: str  # read_at
    jalszv2p: list[Entry[Question]]  # questions

    def forget_form(self) -> None:
        """Forgets one form a site asked."""
        return self.action("forget_form")

    def forget_form_many(self, items: list[ForgetFormArgs]) -> list[dict[str, Any]]:
        """Runs `forget_form` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("forget_form", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"forget_form", "forget_form_many"})


class Question(Resource):
    """Question — `tl.so.tilo.browser.Question`."""
    __surface__ = "so.tilo.browser@172587f7789744b0"
    @property
    def _2wlmb5rp(self) -> str:
        """name (`2wlmb5rp` on the wire)."""
        return self.fields["2wlmb5rp"]

    @_2wlmb5rp.setter
    def _2wlmb5rp(self, value: str) -> None:
        self.edit("2wlmb5rp", value=value)
    z4pmfk6p: str  # answer
    mttccz7q: str  # label
    yk2bhzup: list[str]  # choices
    gmhi7gfy: bool  # required


class Address(Resource):
    """Address — `tl.so.tilo.browser.Address`."""
    __surface__ = "so.tilo.browser@172587f7789744b0"
    fnyioh2y: str  # label
    @property
    def _3ewzn2z4(self) -> str:
        """full_name (`3ewzn2z4` on the wire)."""
        return self.fields["3ewzn2z4"]

    @_3ewzn2z4.setter
    def _3ewzn2z4(self, value: str) -> None:
        self.edit("3ewzn2z4", value=value)
    n2u44pv5: str  # given_name
    lgputt7y: str  # family_name
    plkha23f: str  # street
    pwnlxkv7: str  # city
    g7xpip4v: str  # region
    kj2h5fbf: str  # postal_code
    iciyfupp: str  # country

    def delete_address(self) -> None:
        """Deletes an address, and stops filling forms with it."""
        return self.action("delete_address")

    def delete_address_many(self, items: list[DeleteAddressArgs]) -> list[dict[str, Any]]:
        """Runs `delete_address` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_address", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"delete_address", "delete_address_many"})


class Page(Resource):
    """Page — `tl.so.tilo.browser.Page`."""
    __surface__ = "so.tilo.browser@172587f7789744b0"
    hf6q2oo4: str  # url
    tyhunk32: str  # title
    @property
    def _4nlkcuks(self) -> bool:
        """paused (`4nlkcuks` on the wire)."""
        return self.fields["4nlkcuks"]

    @_4nlkcuks.setter
    def _4nlkcuks(self, value: bool) -> None:
        self.edit("4nlkcuks", value=value)
    s6ryunn5: bool  # private
    dedg3fev: list[Entry[PageStep]]  # steps
    mk755sf3: DialogAsk | None  # dialog
    tveuvbse: MenuAsk | None  # menu
    o2occ235: list[Entry[Download]]  # downloads
    eg4w4fow: bool  # muted
    @property
    def _252payib(self) -> SiteInfo | None:
        """site (`252payib` on the wire)."""
        return self.fields["252payib"]

    @_252payib.setter
    def _252payib(self, value: SiteInfo | None) -> None:
        self.edit("252payib", value=value)
    nfmtkvs7: list[Entry[LoginForm]]  # login_forms
    jbnzv4tj: list[Entry[LoginAccount]]  # login_accounts
    jz6xwcus: str  # capture
    wqz44fsx: str  # login_notice
    ctm6ma5g: list[Entry[HistoryEntry]]  # back
    znzxwmxa: list[Entry[HistoryEntry]]  # forward
    eiqyiw53: bool  # loading
    xu2jvlwq: float  # progress
    @property
    def _5gspmng7(self) -> float:
        """zoom_level (`5gspmng7` on the wire)."""
        return self.fields["5gspmng7"]

    @_5gspmng7.setter
    def _5gspmng7(self, value: float) -> None:
        self.edit("5gspmng7", value=value)
    j5emshom: int  # find_at
    n6y73q7c: int  # find_count

    def read_page(self,
        *,
        from_: int | None = None,
        frame: str | None = None,
    ) -> Answer:
        """What the page of `p` says and does, carried to the window that holds it. Each action answers the page's own report as an [`Answer`], and what was done is written into the page's record. Read the page: what it says, line by line, as an agent reads it. A long page is read in parts: a read that stops short names the line to read on from."""
        args: dict[str, Any] = {}
        if from_ is not None:
            args["from"] = from_
        if frame is not None:
            args["frame"] = frame
        return self.action("read_page", **args)

    def read_page_changes(self,
        *,
        frame: str | None = None,
    ) -> Answer:
        """What changed on the page since the last read: the lines added and removed, or the whole outline when the document is new."""
        args: dict[str, Any] = {}
        if frame is not None:
            args["frame"] = frame
        return self.action("read_page_changes", **args)

    def read_page_element_at(self,
        *,
        x: float,
        y: float,
        frame: str | None = None,
    ) -> Answer:
        """What a click at a point would hit, through overlays and banners, and the element holding it."""
        args: dict[str, Any] = {}
        args["x"] = x
        args["y"] = y
        if frame is not None:
            args["frame"] = frame
        return self.action("read_page_element_at", **args)

    def wait_page_settled(self,
        *,
        quiet_ms: int | None = None,
        timeout_ms: int | None = None,
        text: str | None = None,
        gone: str | None = None,
        frame: str | None = None,
    ) -> Answer:
        """Wait for the page to go quiet, or until the words it must say are there and the ones it must not say are gone."""
        args: dict[str, Any] = {}
        if quiet_ms is not None:
            args["quiet_ms"] = quiet_ms
        if timeout_ms is not None:
            args["timeout_ms"] = timeout_ms
        if text is not None:
            args["text"] = text
        if gone is not None:
            args["gone"] = gone
        if frame is not None:
            args["frame"] = frame
        return self.action("wait_page_settled", **args)

    def read_page_console(self,
        *,
        since: int | None = None,
    ) -> Answer:
        """The page's console messages and uncaught errors after a sequence number."""
        args: dict[str, Any] = {}
        if since is not None:
            args["since"] = since
        return self.action("read_page_console", **args)

    def read_page_network(self,
        *,
        since: int | None = None,
    ) -> Answer:
        """The page's network requests after a sequence number: method, address, type, status and failure, never headers, bodies or cookies."""
        args: dict[str, Any] = {}
        if since is not None:
            args["since"] = since
        return self.action("read_page_network", **args)

    def read_page_record(self,
        *,
        since: int | None = None,
    ) -> Answer:
        """The record of who did what in the page, after the place `since` names."""
        args: dict[str, Any] = {}
        if since is not None:
            args["since"] = since
        return self.action("read_page_record", **args)

    def read_page_fields(self,
        *,
        frame: str | None = None,
    ) -> Answer:
        """Every field of the document with what it holds and who set it. A secret is listed by length."""
        args: dict[str, Any] = {}
        if frame is not None:
            args["frame"] = frame
        return self.action("read_page_fields", **args)

    def read_page_forms(self,
        *,
        frame: str | None = None,
    ) -> Answer:
        """The document's forms as what they ask: their questions, the answers they offer, which are required, and the buttons that send them."""
        args: dict[str, Any] = {}
        if frame is not None:
            args["frame"] = frame
        return self.action("read_page_forms", **args)

    def read_page_table(self,
        *,
        element: str | None = None,
        from_: int | None = None,
        frame: str | None = None,
    ) -> Answer:
        """The page's tables as rows, a long one read in parts."""
        args: dict[str, Any] = {}
        if element is not None:
            args["element"] = element
        if from_ is not None:
            args["from"] = from_
        if frame is not None:
            args["frame"] = frame
        return self.action("read_page_table", **args)

    def find_page_text(self,
        *,
        text: str,
        frame: str | None = None,
        frames: bool | None = None,
    ) -> Answer:
        """The outline lines that hold `text`, each with its line number."""
        args: dict[str, Any] = {}
        args["text"] = text
        if frame is not None:
            args["frame"] = frame
        if frames is not None:
            args["frames"] = frames
        return self.action("find_page_text", **args)

    def find_page_element(self,
        *,
        role: str | None = None,
        name: str | None = None,
        nth: int | None = None,
        frame: str | None = None,
        frames: bool | None = None,
    ) -> Answer:
        """The element the role and name name, and the ref that acts there."""
        args: dict[str, Any] = {}
        if role is not None:
            args["role"] = role
        if name is not None:
            args["name"] = name
        if nth is not None:
            args["nth"] = nth
        if frame is not None:
            args["frame"] = frame
        if frames is not None:
            args["frames"] = frames
        return self.action("find_page_element", **args)

    def click_page(self,
        *,
        element: str,
        frame: str | None = None,
    ) -> Answer:
        """Press the element, as a hand does."""
        args: dict[str, Any] = {}
        args["element"] = element
        if frame is not None:
            args["frame"] = frame
        return self.action("click_page", **args)

    def hover_page(self,
        *,
        element: str | None = None,
        x: float | None = None,
        y: float | None = None,
    ) -> Answer:
        """Move the pointer over an element, or to a point of the page."""
        args: dict[str, Any] = {}
        if element is not None:
            args["element"] = element
        if x is not None:
            args["x"] = x
        if y is not None:
            args["y"] = y
        return self.action("hover_page", **args)

    def drag_page(self,
        *,
        from_: str | None = None,
        from_x: float | None = None,
        from_y: float | None = None,
        to: str | None = None,
        to_x: float | None = None,
        to_y: float | None = None,
        steps: int | None = None,
    ) -> Answer:
        """Drag from one element or point to another, in steps."""
        args: dict[str, Any] = {}
        if from_ is not None:
            args["from"] = from_
        if from_x is not None:
            args["from_x"] = from_x
        if from_y is not None:
            args["from_y"] = from_y
        if to is not None:
            args["to"] = to
        if to_x is not None:
            args["to_x"] = to_x
        if to_y is not None:
            args["to_y"] = to_y
        if steps is not None:
            args["steps"] = steps
        return self.action("drag_page", **args)

    def fill_page(self,
        *,
        element: str,
        text: str,
        frame: str | None = None,
    ) -> Answer:
        """Write `text` into an editable element."""
        args: dict[str, Any] = {}
        args["element"] = element
        args["text"] = text
        if frame is not None:
            args["frame"] = frame
        return self.action("fill_page", **args)

    def fill_page_secret(self,
        *,
        element: str,
        text: str,
        frame: str | None = None,
    ) -> Answer:
        """Write a secret into a password or card field: it is never kept in the page's record."""
        args: dict[str, Any] = {}
        args["element"] = element
        args["text"] = text
        if frame is not None:
            args["frame"] = frame
        return self.action("fill_page_secret", **args)

    def select_page(self,
        *,
        element: str,
        values: list[str],
        frame: str | None = None,
    ) -> Answer:
        """Choose options of a select."""
        args: dict[str, Any] = {}
        args["element"] = element
        args["values"] = values
        if frame is not None:
            args["frame"] = frame
        return self.action("select_page", **args)

    def fill_page_files(self,
        *,
        element: str,
        paths: list[str],
        frame: str | None = None,
    ) -> Answer:
        """Choose the files of a file input, by absolute path."""
        args: dict[str, Any] = {}
        args["element"] = element
        args["paths"] = paths
        if frame is not None:
            args["frame"] = frame
        return self.action("fill_page_files", **args)

    def press_page(self,
        *,
        key: str,
        element: str | None = None,
        frame: str | None = None,
    ) -> Answer:
        """Press a key in the page, as `Control+a` or `Enter`."""
        args: dict[str, Any] = {}
        args["key"] = key
        if element is not None:
            args["element"] = element
        if frame is not None:
            args["frame"] = frame
        return self.action("press_page", **args)

    def submit_page(self,
        *,
        element: str,
        cross_origin: bool | None = None,
        frame: str | None = None,
    ) -> Answer:
        """Send the element's form through the page's own validation."""
        args: dict[str, Any] = {}
        args["element"] = element
        if cross_origin is not None:
            args["cross_origin"] = cross_origin
        if frame is not None:
            args["frame"] = frame
        return self.action("submit_page", **args)

    def update_download(self,
        *,
        id: int,
        state: str,
        name: str,
        url: str,
        path: str,
        retryable: bool,
        received: int,
        total: int,
        error: str,
    ) -> None:
        """Writes what the page last said of one of its downloads into the page's record, as its window hands it over while the transfer moves. The window raises it, never a person, agent or script."""
        return self.action("update_download", id=id, state=state, name=name, url=url, path=path, retryable=retryable, received=received, total=total, error=error)

    def update_download_many(self, items: list[UpdateDownloadArgs]) -> list[dict[str, Any]]:
        """Runs `update_download` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("update_download", items)

    def open_from_page(self,
        *,
        url: str,
        popup: bool,
    ) -> None:
        """Keeps a page the page opened in a window of its own as a page of this browser, as its window hands it over: a popup a site sized, or a plain new tab."""
        return self.action("open_from_page", url=url, popup=popup)

    def open_from_page_many(self, items: list[OpenFromPageArgs]) -> list[dict[str, Any]]:
        """Runs `open_from_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("open_from_page", items)

    def remember_history(self,
        *,
        direction: str,
        entries: str,
    ) -> None:
        """Writes the pages the page can go to one way into its record, as its window hands them over: what the menu then offers beside Back and Forward."""
        return self.action("remember_history", direction=direction, entries=entries)

    def remember_history_many(self, items: list[RememberHistoryArgs]) -> list[dict[str, Any]]:
        """Runs `remember_history` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remember_history", items)

    def fill_page_form(self,
        *,
        form: str,
        answers: dict[str, str],
        submit: bool | None = None,
        cross_origin: bool | None = None,
        frame: str | None = None,
    ) -> Answer:
        """Answers a whole form of the page in one ask: each question's answer by its name, through the page's own validation, and the form's first button when `submit` is on. A form is named by the shape `read_page_forms` publishes for it."""
        args: dict[str, Any] = {}
        args["form"] = form
        args["answers"] = answers
        if submit is not None:
            args["submit"] = submit
        if cross_origin is not None:
            args["cross_origin"] = cross_origin
        if frame is not None:
            args["frame"] = frame
        return self.action("fill_page_form", **args)

    def pointer_page(self,
        *,
        steps: list[str],
        button: str | None = None,
    ) -> Answer:
        """A trusted pointer gesture: press at the first point, move through the rest and release at the last, for drawing on canvases."""
        args: dict[str, Any] = {}
        args["steps"] = steps
        if button is not None:
            args["button"] = button
        return self.action("pointer_page", **args)

    def scroll_page(self,
        *,
        element: str | None = None,
        to: str | None = None,
        by: int | None = None,
        into_view: str | None = None,
        frame: str | None = None,
    ) -> Answer:
        """Turn the page's wheel to the top or the bottom, into view, or by a distance."""
        args: dict[str, Any] = {}
        if element is not None:
            args["element"] = element
        if to is not None:
            args["to"] = to
        if by is not None:
            args["by"] = by
        if into_view is not None:
            args["into_view"] = into_view
        if frame is not None:
            args["frame"] = frame
        return self.action("scroll_page", **args)

    def capture_page(self,
        *,
        element: str | None = None,
        max_width: int | None = None,
        frame: str | None = None,
    ) -> Answer:
        """A picture of the page, or of one element's box, as a PNG."""
        args: dict[str, Any] = {}
        if element is not None:
            args["element"] = element
        if max_width is not None:
            args["max_width"] = max_width
        if frame is not None:
            args["frame"] = frame
        return self.action("capture_page", **args)

    def navigate(self, *, url: str) -> Answer:
        """Open an address in the page's own window."""
        return self.action("navigate", url=url)

    def back(self) -> Answer:
        """Go back to the page before this one."""
        return self.action("back")

    def forward(self) -> Answer:
        """Go forward to the page this one followed."""
        return self.action("forward")

    def reload(self) -> Answer:
        """Load the page again."""
        return self.action("reload")

    def stop_loading(self) -> Answer:
        """Stop loading the page."""
        return self.action("stop_loading")

    def reader(self, *, enabled: bool) -> Answer:
        """Show the page's article without its clutter."""
        return self.action("reader", enabled=enabled)

    def zoom(self, *, level: float) -> Answer:
        """Show the page at `level` zoom, where 1 is actual size."""
        return self.action("zoom", level=level)

    def find_in_page(self,
        *,
        text: str,
        forward: bool | None = None,
        next: bool | None = None,
    ) -> Answer:
        """Find words in the page, as the find bar does."""
        args: dict[str, Any] = {}
        args["text"] = text
        if forward is not None:
            args["forward"] = forward
        if next is not None:
            args["next"] = next
        return self.action("find_in_page", **args)

    def stop_finding(self) -> Answer:
        """Close the find bar's search."""
        return self.action("stop_finding")

    def answer_page_dialog(self,
        *,
        id: int,
        accept: bool,
        text: str | None = None,
    ) -> Answer:
        """Answer a page's own question: an alert, a confirmation or a prompt."""
        args: dict[str, Any] = {}
        args["id"] = id
        args["accept"] = accept
        if text is not None:
            args["text"] = text
        return self.action("answer_page_dialog", **args)

    def refresh_page(self) -> Answer:
        """Reads what the page waits on into its window's record: the question it asks, the menu it showed, and how far it has loaded. The window calls it when it opens the page and after each answer; nothing here asks the page anything of its own."""
        return self.action("refresh_page")

    def allow_permission(self,
        *,
        id: int,
        origin: str,
        wants: str,
    ) -> Answer:
        """Lets a site use what it asked for (its camera, microphone, location, notifications). The window raises it as the Browser's own call when the site asks: the person allows it on the system's card, which names the Browser and the site, and an always-allow lets this site have it from then on. Declined, or the page gone, the site is told no."""
        return self.action("allow_permission", id=id, origin=origin, wants=wants)

    def save_download(self,
        *,
        id: int,
        name: str,
        url: str,
    ) -> Answer:
        """Saves the file a page offers. The window raises it as the Browser's own call when the page offers one, and the person allows it on the system's card."""
        return self.action("save_download", id=id, name=name, url=url)

    def discard_download(self,
        *,
        id: int,
        name: str | None = None,
    ) -> Answer:
        """Turns down the file a page offers: nothing is saved, and the page is told so."""
        args: dict[str, Any] = {}
        args["id"] = id
        if name is not None:
            args["name"] = name
        return self.action("discard_download", **args)

    def download(self, *, url: str) -> Answer:
        """Saves what an address serves into the browser's downloads."""
        return self.action("download", url=url)

    def cancel_download(self, *, d: Download) -> Answer:
        """Stops a download that is still going."""
        return self.action("cancel_download", d=d)

    def retry_download(self, *, d: Download) -> Answer:
        """Starts a failed download again from the beginning."""
        return self.action("retry_download", d=d)

    def cancel_all_downloads(self) -> Answer:
        """Stops every download this page has going."""
        return self.action("cancel_all_downloads")

    def developer_tools(self) -> Answer:
        """Opens the engine's developer tools for this page: its console, its elements and its network."""
        return self.action("developer_tools")

    def mute(self,
        *,
        muted: bool | None = None,
    ) -> Answer:
        """Silences the page's sound, or lets it play again."""
        args: dict[str, Any] = {}
        if muted is not None:
            args["muted"] = muted
        return self.action("mute", **args)

    def read_back_history(self) -> Answer:
        """Asks the page where it can go back to. The engine answers with the pages before this one, which the page's record holds once its window reads them again."""
        return self.action("read_back_history")

    def read_forward_history(self) -> Answer:
        """Asks the page where it can go forward to, the answer kept like [`read_back_history`]'s."""
        return self.action("read_forward_history")

    def go_history_entry(self, *, entry: int) -> Answer:
        """Goes to one page of the page's own history, by the entry its reading named."""
        return self.action("go_history_entry", entry=entry)

    def read_site(self) -> Answer:
        """Reads the site's connection, its certificate, the requests tracker protection blocked and what the site may ask for, into the page's record."""
        return self.action("read_site")

    def clear_site_data(self) -> Answer:
        """Clears this site's storage, cookies and remembered forms, including cookies it shares with its subdomains, and reloads it."""
        return self.action("clear_site_data")

    def block_site_permissions(self) -> Answer:
        """Blocks what the site may ask for (its camera, microphone, location and notifications)."""
        return self.action("block_site_permissions")

    def ask_site_permissions_again(self) -> Answer:
        """Lets the site ask again for what it was refused."""
        return self.action("ask_site_permissions_again")

    def discover_logins(self) -> Answer:
        """Reads the login forms this page asks for, through the engine's own credential store."""
        return self.action("discover_logins")

    def list_saved_logins(self) -> Answer:
        """Lists the logins the engine keeps for this site."""
        return self.action("list_saved_logins")

    def capture_login(self, *, f: LoginForm) -> Answer:
        """Takes what the person typed into a login form, so it can be saved for this site. The password stays in the engine's credential store and never crosses to the app."""
        return self.action("capture_login", f=f)

    def save_login(self) -> Answer:
        """Saves the capture the engine took for this site."""
        return self.action("save_login")

    def fill_saved_login(self,
        *,
        f: LoginForm,
        a: LoginAccount,
    ) -> Answer:
        """Fills a form of this page with a login the engine keeps."""
        return self.action("fill_saved_login", f=f, a=a)

    def update_saved_login(self,
        *,
        f: LoginForm,
        a: LoginAccount,
    ) -> Answer:
        """Replaces a saved login with the capture the engine took."""
        return self.action("update_saved_login", f=f, a=a)

    def delete_saved_login(self, *, a: LoginAccount) -> Answer:
        """Forgets a login the engine keeps for this site."""
        return self.action("delete_saved_login", a=a)

    def cancel_login(self) -> Answer:
        """Turns down the login the engine offers to save."""
        return self.action("cancel_login")

    def discover_addresses(self) -> Answer:
        """Reads the addresses the engine fills forms with."""
        return self.action("discover_addresses")

    def fill_saved_address(self,
        *,
        f: LoginForm,
        a: Address,
    ) -> Answer:
        """Fills a form of this page with a saved address, through the engine."""
        return self.action("fill_saved_address", f=f, a=a)

    def list_extensions(self) -> Answer:
        """Reads the extensions installed on this device into the Browser's record."""
        return self.action("list_extensions")

    def install_extension(self, *, path: str) -> Answer:
        """Installs the extension package at `path`, a .crx signed by its publisher or a plain .zip: the engine verifies a signed package, unpacks it under its own identifier, and lists it."""
        return self.action("install_extension", path=path)

    def enable_extension(self,
        *,
        e: Extension,
        on: bool | None = None,
    ) -> Answer:
        """Turns an installed extension on, or off: Chromium loads the ones that are on when a page's window starts."""
        args: dict[str, Any] = {}
        args["e"] = e
        if on is not None:
            args["on"] = on
        return self.action("enable_extension", **args)

    def remove_extension(self, *, e: Extension) -> Answer:
        """Forgets an extension installed on this device: its folder and everything it may do."""
        return self.action("remove_extension", e=e)

    def remember_extensions(self,
        *,
        installed: str,
        message: str,
    ) -> None:
        """Writes the extensions a page's window listed into the Browser's record. The window raises it; a person, agent or script never does."""
        return self.action("remember_extensions", installed=installed, message=message)

    def remember_extensions_many(self, items: list[RememberExtensionsArgs]) -> list[dict[str, Any]]:
        """Runs `remember_extensions` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remember_extensions", items)

    def note_extension(self,
        *,
        ok: bool,
        kind: str,
        message: str,
        id: str,
    ) -> None:
        """Writes what installing, turning on or removing an extension came to, in the person's words. The window raises it; a person, agent or script never does."""
        return self.action("note_extension", ok=ok, kind=kind, message=message, id=id)

    def note_extension_many(self, items: list[NoteExtensionArgs]) -> list[dict[str, Any]]:
        """Runs `note_extension` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("note_extension", items)

    def remember_credential(self,
        *,
        did: str,
        success: bool,
        message: str,
        forms: str,
        accounts: str,
        capture: str,
        username: str,
        late: bool,
    ) -> None:
        """Writes what the engine's credential store answered into the page's record: the forms and accounts it listed, the capture it took, and its own words. The window raises it; a person, agent or script never does."""
        return self.action("remember_credential", did=did, success=success, message=message, forms=forms, accounts=accounts, capture=capture, username=username, late=late)

    def remember_credential_many(self, items: list[RememberCredentialArgs]) -> list[dict[str, Any]]:
        """Runs `remember_credential` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remember_credential", items)

    def sync_site_exceptions(self) -> Answer:
        """Tells the engine which sites tracker protection is off for, as the Browser's own set says: the engine keeps the same list beside its filters."""
        return self.action("sync_site_exceptions")

    def allow_extension_access(self,
        *,
        id: int,
        extension: str,
        could: str,
    ) -> Answer:
        """Gives an extension the more access it asked for. The window raises it as the Browser's own call, and the person allows it on the system's card, which says what the extension could then do."""
        return self.action("allow_extension_access", id=id, extension=extension, could=could)

    def sign_in(self,
        *,
        id: int,
        asker: str,
        site: str,
        realm: str,
    ) -> Answer:
        """Signs in to a site that asks for a user name and a password. The window raises it as the Browser's own call; the person types both on the system's card, on this device, and the password goes to the site alone: never into this action's arguments, its record or history."""
        return self.action("sign_in", id=id, asker=asker, site=site, realm=realm)

    def cancel_sign_in(self,
        *,
        id: int,
        asker: str,
    ) -> Answer:
        """Turns down a site's sign-in: nothing is sent, and the server shows its own page instead."""
        return self.action("cancel_sign_in", id=id, asker=asker)

    def let_leave(self,
        *,
        id: int,
        origin: str,
        method: str,
        leaving: str,
    ) -> Answer:
        """Lets what the page sends leave it: a form, a navigation or a request kept until the person says so. The window raises it as the Browser's own call, and the person allows it on the system's card, which says what leaves and where."""
        return self.action("let_leave", id=id, origin=origin, method=method, leaving=leaving)

    def context_menu_action(self,
        *,
        id: int,
        command: int,
    ) -> Answer:
        """Takes a row of the page's own menu, by the command the engine gave it."""
        return self.action("context_menu_action", id=id, command=command)

    def fill_address(self,
        *,
        form: str,
        a: Address,
    ) -> Answer:
        """Fills a form of the page with a saved address, question by question, without sending it. The form is named by the shape its reading publishes (`RememberedForm.signature`, or the signature `read_page_forms` answers), and the page's own validation still decides whether it could be sent."""
        return self.action("fill_address", form=form, a=a)

    def went_to(self, *, url: str) -> None:
        """The page went somewhere: where it is now is written on the page, and the visit joins the person's history unless the page's window paused it."""
        return self.action("went_to", url=url)

    def went_to_many(self, items: list[WentToArgs]) -> list[dict[str, Any]]:
        """Runs `went_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("went_to", items)

    def toggle_pause(self,
        *,
        paused: bool | None = None,
    ) -> None:
        """Pauses or resumes the person's history in a page's window."""
        args: dict[str, Any] = {}
        if paused is not None:
            args["paused"] = paused
        return self.action("toggle_pause", **args)

    def toggle_pause_many(self, items: list[TogglePauseArgs]) -> list[dict[str, Any]]:
        """Runs `toggle_pause` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("toggle_pause", items)

    def ask_page(self, *, request: str) -> Answer:
        """The page `p` does what `request` (JSON, as the window's page reads it) says, and answers the page's own report. What was done is written into the page's record; a call that could not be made answers with its words. Every named page action below is this one, written out."""
        return self.action("ask_page", request=request)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"read_page", "read_page_changes", "read_page_element_at", "wait_page_settled", "read_page_console", "read_page_network", "read_page_record", "read_page_fields", "read_page_forms", "read_page_table", "find_page_text", "find_page_element", "click_page", "hover_page", "drag_page", "fill_page", "fill_page_secret", "select_page", "fill_page_files", "press_page", "submit_page", "update_download", "update_download_many", "open_from_page", "open_from_page_many", "remember_history", "remember_history_many", "fill_page_form", "pointer_page", "scroll_page", "capture_page", "navigate", "back", "forward", "reload", "stop_loading", "reader", "zoom", "find_in_page", "stop_finding", "answer_page_dialog", "refresh_page", "allow_permission", "save_download", "discard_download", "download", "cancel_download", "retry_download", "cancel_all_downloads", "developer_tools", "mute", "read_back_history", "read_forward_history", "go_history_entry", "read_site", "clear_site_data", "block_site_permissions", "ask_site_permissions_again", "discover_logins", "list_saved_logins", "capture_login", "save_login", "fill_saved_login", "update_saved_login", "delete_saved_login", "cancel_login", "discover_addresses", "fill_saved_address", "list_extensions", "install_extension", "enable_extension", "remove_extension", "remember_extensions", "remember_extensions_many", "note_extension", "note_extension_many", "remember_credential", "remember_credential_many", "sync_site_exceptions", "allow_extension_access", "sign_in", "cancel_sign_in", "let_leave", "context_menu_action", "fill_address", "went_to", "went_to_many", "toggle_pause", "toggle_pause_many", "ask_page"})


class PageStep(Resource):
    """PageStep — `tl.so.tilo.browser.PageStep`."""
    __surface__ = "so.tilo.browser@172587f7789744b0"
    cpjp36fw: int  # at
    @property
    def _4mcq24cu(self) -> str | None:
        """actor (`4mcq24cu` on the wire)."""
        return self.fields["4mcq24cu"]

    @_4mcq24cu.setter
    def _4mcq24cu(self, value: str | None) -> None:
        self.edit("4mcq24cu", value=value)
    g4lc2ulh: str  # device
    ouuwq4an: str  # device_name
    @property
    def _7qikcw5o(self) -> str:
        """operation (`7qikcw5o` on the wire)."""
        return self.fields["7qikcw5o"]

    @_7qikcw5o.setter
    def _7qikcw5o(self, value: str) -> None:
        self.edit("7qikcw5o", value=value)
    olgrtknj: bool  # ok
    @property
    def _2mfaldtw(self) -> str:
        """code (`2mfaldtw` on the wire)."""
        return self.fields["2mfaldtw"]

    @_2mfaldtw.setter
    def _2mfaldtw(self, value: str) -> None:
        self.edit("2mfaldtw", value=value)
    grbghvzm: str  # message
    vlqrueqe: str  # url
    x3xrjcbr: str  # to_url
    ta3jd45o: str  # role
    kmz5bb2s: str  # element
    g67lnb6k: str | None  # text
    ws5snl25: list[str] | None  # values


class DialogAsk(Resource):
    """DialogAsk — `tl.so.tilo.browser.DialogAsk`."""
    __surface__ = "so.tilo.browser@172587f7789744b0"
    trchguwk: int  # request
    plkldhkl: str  # kind
    ruephqmr: str  # origin
    asepg6kx: str  # message
    @property
    def _4v2f3ua3(self) -> str:
        """default_text (`4v2f3ua3` on the wire)."""
        return self.fields["4v2f3ua3"]

    @_4v2f3ua3.setter
    def _4v2f3ua3(self, value: str) -> None:
        self.edit("4v2f3ua3", value=value)


class MenuAsk(Resource):
    """MenuAsk — `tl.so.tilo.browser.MenuAsk`."""
    __surface__ = "so.tilo.browser@172587f7789744b0"
    hjqv4o64: int  # request
    f2gbwl6v: str  # link
    @property
    def _6b3vpys4(self) -> list[MenuRow]:
        """items (`6b3vpys4` on the wire)."""
        return self.fields["6b3vpys4"]

    @_6b3vpys4.setter
    def _6b3vpys4(self, value: list[MenuRow]) -> None:
        self.edit("6b3vpys4", value=value)


class MenuRow(Resource):
    """MenuRow — `tl.so.tilo.browser.MenuRow`."""
    __surface__ = "so.tilo.browser@172587f7789744b0"
    kybyncey: int  # command
    cgfpo6b7: str  # label
    hn5vxavq: bool  # disabled
    qmekhn2b: bool  # checked
    @property
    def _67scpsna(self) -> bool:
        """separator (`67scpsna` on the wire)."""
        return self.fields["67scpsna"]

    @_67scpsna.setter
    def _67scpsna(self, value: bool) -> None:
        self.edit("67scpsna", value=value)


class Download(Resource):
    """Download — `tl.so.tilo.browser.Download`."""
    __surface__ = "so.tilo.browser@172587f7789744b0"
    anfisgsq: int  # transfer
    in525zqp: str  # state
    wz6wi2p3: str  # name
    z4gzfvzw: str  # url
    feka5n4y: str  # path
    kubpqqew: bool  # retryable
    cokrfftg: int  # received
    pbavpdyk: int  # total
    wfaksaa6: str  # error


class SiteInfo(Resource):
    """SiteInfo — `tl.so.tilo.browser.SiteInfo`."""
    __surface__ = "so.tilo.browser@172587f7789744b0"
    hrkhpkbd: str  # origin
    scje2sv3: int  # generation
    lyopffwi: int  # blocked
    az2v2pr3: bool  # protected
    ageqt2tb: str  # security
    i3vtfymr: int  # certificate
    zarq7l6o: str  # issuer
    @property
    def _5nsqo5b5(self) -> str:
        """subject (`5nsqo5b5` on the wire)."""
        return self.fields["5nsqo5b5"]

    @_5nsqo5b5.setter
    def _5nsqo5b5(self, value: str) -> None:
        self.edit("5nsqo5b5", value=value)
    gsqr2kkr: list[str]  # permissions
    eygtkatk: bool  # can_reset


class HistoryEntry(Resource):
    """HistoryEntry — `tl.so.tilo.browser.HistoryEntry`."""
    __surface__ = "so.tilo.browser@172587f7789744b0"
    eyk6uw4k: int  # entry
    @property
    def _2chmpqlt(self) -> str:
        """url (`2chmpqlt` on the wire)."""
        return self.fields["2chmpqlt"]

    @_2chmpqlt.setter
    def _2chmpqlt(self, value: str) -> None:
        self.edit("2chmpqlt", value=value)
    uugq5btr: str  # title


class Extension(Resource):
    """Extension — `tl.so.tilo.browser.Extension`."""
    __surface__ = "so.tilo.browser@172587f7789744b0"
    iymjskx5: str  # key
    vhst373v: str  # name
    @property
    def _7b7zbela(self) -> str:
        """version (`7b7zbela` on the wire)."""
        return self.fields["7b7zbela"]

    @_7b7zbela.setter
    def _7b7zbela(self, value: str) -> None:
        self.edit("7b7zbela", value=value)
    v7jkztuy: int  # manifest_version
    iaycfmmk: str  # description
    kkeg7wgf: str  # permissions
    @property
    def _372chbvw(self) -> bool:
        """enabled (`372chbvw` on the wire)."""
        return self.fields["372chbvw"]

    @_372chbvw.setter
    def _372chbvw(self, value: bool) -> None:
        self.edit("372chbvw", value=value)


class LoginForm(Resource):
    """LoginForm — `tl.so.tilo.browser.LoginForm`."""
    __surface__ = "so.tilo.browser@172587f7789744b0"
    hdxbxfcl: str  # form
    enz3azev: str  # username
    kqqay3gr: str  # kind


class LoginAccount(Resource):
    """LoginAccount — `tl.so.tilo.browser.LoginAccount`."""
    __surface__ = "so.tilo.browser@172587f7789744b0"
    @property
    def _53raewop(self) -> str:
        """account (`53raewop` on the wire)."""
        return self.fields["53raewop"]

    @_53raewop.setter
    def _53raewop(self, value: str) -> None:
        self.edit("53raewop", value=value)
    @property
    def _5oqresy2(self) -> str:
        """username (`5oqresy2` on the wire)."""
        return self.fields["5oqresy2"]

    @_5oqresy2.setter
    def _5oqresy2(self, value: str) -> None:
        self.edit("5oqresy2", value=value)


class Ask(Resource):
    """Ask — `tl.so.tilo.browser.Ask`."""
    __surface__ = "so.tilo.browser@172587f7789744b0"
    fqvhxdyr: str  # op
    hfpnr6ua: str | None  # element
    tpz54oe5: str | None  # text
    @property
    def _5vdpb6hg(self) -> int | None:
        """from (`5vdpb6hg` on the wire)."""
        return self.fields["5vdpb6hg"]

    @_5vdpb6hg.setter
    def _5vdpb6hg(self, value: int | None) -> None:
        self.edit("5vdpb6hg", value=value)
    wf32vfhg: int | None  # by
    q76mm3tr: str | None  # drag_from
    byeflqne: int | None  # drag_steps
    hxjdnlwy: str | None  # frame
    @property
    def _4udfayix(self) -> bool:
        """frames (`4udfayix` on the wire)."""
        return self.fields["4udfayix"]

    @_4udfayix.setter
    def _4udfayix(self, value: bool) -> None:
        self.edit("4udfayix", value=value)
    txp5lrru: float | None  # x
    bqydqtva: float | None  # y
    wbhz4hh6: float | None  # from_x
    iuschp5g: float | None  # from_y
    @property
    def _6i5bwl43(self) -> float | None:
        """to_x (`6i5bwl43` on the wire)."""
        return self.fields["6i5bwl43"]

    @_6i5bwl43.setter
    def _6i5bwl43(self, value: float | None) -> None:
        self.edit("6i5bwl43", value=value)
    hrquaspl: float | None  # to_y
    @property
    def _5ka7kek5(self) -> str | None:
        """role (`5ka7kek5` on the wire)."""
        return self.fields["5ka7kek5"]

    @_5ka7kek5.setter
    def _5ka7kek5(self, value: str | None) -> None:
        self.edit("5ka7kek5", value=value)
    davmrr4h: str | None  # name
    dmxbh4fm: int | None  # nth
    ao4elp2a: str | None  # key
    jmhhkxjz: str | None  # url
    @property
    def _2l2hjzlp(self) -> str | None:
        """path (`2l2hjzlp` on the wire)."""
        return self.fields["2l2hjzlp"]

    @_2l2hjzlp.setter
    def _2l2hjzlp(self, value: str | None) -> None:
        self.edit("2l2hjzlp", value=value)
    @property
    def _5px2ripq(self) -> str | None:
        """extension (`5px2ripq` on the wire)."""
        return self.fields["5px2ripq"]

    @_5px2ripq.setter
    def _5px2ripq(self, value: str | None) -> None:
        self.edit("5px2ripq", value=value)
    lqvkggvx: float | None  # level
    zroiv7hs: list[str] | None  # values
    ntpmcicu: dict[str, str] | None  # answers
    zoqmh2dz: bool  # muted
    l3gmhwrx: str | None  # direction
    pucrebly: int | None  # entry
    @property
    def _4vqdzqut(self) -> str | None:
        """doing (`4vqdzqut` on the wire)."""
        return self.fields["4vqdzqut"]

    @_4vqdzqut.setter
    def _4vqdzqut(self, value: str | None) -> None:
        self.edit("4vqdzqut", value=value)
    fbtgqfhx: list[str] | None  # origins
    ymm5jh2h: str | None  # origin
    wdlbrp6r: str | None  # credential
    x6klrsfn: str | None  # capture
    o345kv2f: str | None  # account
    cirolhk7: Address | None  # address
    @property
    def _7gcvts2u(self) -> int | None:
        """generation (`7gcvts2u` on the wire)."""
        return self.fields["7gcvts2u"]

    @_7gcvts2u.setter
    def _7gcvts2u(self, value: int | None) -> None:
        self.edit("7gcvts2u", value=value)
    ky26jv4a: list[str] | None  # paths
    iabkjt2r: list[str] | None  # steps
    zh65uqqk: int | None  # since
    ersdn54t: int | None  # quiet_ms
    bgflyuoe: int | None  # timeout_ms
    hjfilxdg: str | None  # gone
    btx3fn7s: bool  # cross_origin
    zf6y6r3k: bool  # secret
    arhpr4t7: bool  # submit
    h6fxerpb: str | None  # form
    cnvdpt2t: str | None  # submit_button
    asha7lz3: str | None  # to
    jcrv3nzs: str | None  # into_view
    qit2hijp: str | None  # button
    @property
    def _2zpkv6jg(self) -> bool:
        """enabled (`2zpkv6jg` on the wire)."""
        return self.fields["2zpkv6jg"]

    @_2zpkv6jg.setter
    def _2zpkv6jg(self, value: bool) -> None:
        self.edit("2zpkv6jg", value=value)
    fggdqfhy: int | None  # command
    sizwtymn: int | None  # max_width
    htce2xyp: int | None  # id
    wbwndzhw: str | None  # asker
    nqkenrjw: str | None  # username
    ns2mfzwr: str | None  # password
    qvvu7twk: bool  # typed
    vwi6rjsr: bool  # accept
    szlyr265: bool  # allow
    acxwp3qh: bool  # forward
    m6sqrzav: bool  # next


class Answer(Resource):
    """Answer — `tl.so.tilo.browser.Answer`."""
    __surface__ = "so.tilo.browser@172587f7789744b0"
    @property
    def _2ubdeo5l(self) -> bool:
        """ok (`2ubdeo5l` on the wire)."""
        return self.fields["2ubdeo5l"]

    @_2ubdeo5l.setter
    def _2ubdeo5l(self, value: bool) -> None:
        self.edit("2ubdeo5l", value=value)
    pv4ylrjw: str  # code
    d7izzogo: str  # message
    ta6htvhh: str  # url
    xzex263o: str  # title
    htwzro7c: str  # navigation
    vptvuat4: bool  # loading
    vvykamug: str  # outline
    i2okiki6: str  # element
    mmtsrl7r: int  # line
    p6uplgxr: str  # role
    @property
    def _7zlc443q(self) -> str:
        """name (`7zlc443q` on the wire)."""
        return self.fields["7zlc443q"]

    @_7zlc443q.setter
    def _7zlc443q(self, value: str) -> None:
        self.edit("7zlc443q", value=value)
    @property
    def _2ibb6fxi(self) -> str:
        """frame (`2ibb6fxi` on the wire)."""
        return self.fields["2ibb6fxi"]

    @_2ibb6fxi.setter
    def _2ibb6fxi(self, value: str) -> None:
        self.edit("2ibb6fxi", value=value)
    cxsstht7: str  # image


class Browser(Resource):
    """Browser — `tl.so.tilo.browser.Browser`."""
    __surface__ = "so.tilo.browser@172587f7789744b0"
    v73p6lyt: bool  # adblock
    @property
    def _4mcclqi3(self) -> bool:
        """clear_on_exit (`4mcclqi3` on the wire)."""
        return self.fields["4mcclqi3"]

    @_4mcclqi3.setter
    def _4mcclqi3(self, value: bool) -> None:
        self.edit("4mcclqi3", value=value)
    zhpmqsvs: str  # search_engine
    ezeliuhm: str  # homepage
    @property
    def _54pfa4xp(self) -> str:
        """trouble (`54pfa4xp` on the wire)."""
        return self.fields["54pfa4xp"]

    @_54pfa4xp.setter
    def _54pfa4xp(self, value: str) -> None:
        self.edit("54pfa4xp", value=value)
    b7avacvg: list[str]  # sites_without_protection
    @property
    def _5kcrev3j(self) -> list[Entry[Bookmark]]:
        """bookmarks (`5kcrev3j` on the wire)."""
        return self.fields["5kcrev3j"]

    @_5kcrev3j.setter
    def _5kcrev3j(self, value: list[Entry[Bookmark]]) -> None:
        self.edit("5kcrev3j", value=value)
    qfwsjkyv: list[Entry[Visit]]  # visits
    dlo6rb3l: list[Entry[ImportedVisit]]  # imported_visits
    m7eeditk: list[Entry[RememberedForm]]  # forms
    @property
    def _2zm7y6e5(self) -> list[Entry[Address]]:
        """addresses (`2zm7y6e5` on the wire)."""
        return self.fields["2zm7y6e5"]

    @_2zm7y6e5.setter
    def _2zm7y6e5(self, value: list[Entry[Address]]) -> None:
        self.edit("2zm7y6e5", value=value)
    zx4a3bhv: list[Entry[Page]]  # pages
    z3z5vks2: bool  # addresses_enabled
    nskkfe4o: list[Entry[Extension]]  # extensions
    uq4ctcke: str  # extension_notice

    def new_page(self) -> Page:
        """Opens a page in a window of its own. Every window of the app is one page; what it shows, its address and its own back and forward are the system's."""
        return self.action("new_page")

    def new_private_page(self) -> Page:
        """Opens a window that browses privately: nothing it reads, signs in to or downloads joins what this device keeps of the person."""
        return self.action("new_private_page")

    def go(self, *, typed: str) -> Page:
        """Opens an address in a page of its own, or asks the search engine for what was typed when it is not one."""
        return self.action("go", typed=typed)

    def bookmark(self,
        *,
        url: str,
        title: str | None = None,
    ) -> None:
        """Saves a page for later. A page is bookmarked by its address alone: bookmarking it twice keeps one bookmark, and no title given falls back to the address."""
        args: dict[str, Any] = {}
        args["url"] = url
        if title is not None:
            args["title"] = title
        return self.action("bookmark", **args)

    def bookmark_many(self, items: list[BookmarkArgs]) -> list[dict[str, Any]]:
        """Runs `bookmark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("bookmark", items)

    def set_site_protection(self,
        *,
        origin: str,
        enabled: bool,
    ) -> None:
        """Turns tracker protection off for one site, or back on. A site is named by its origin, so turning it off for `https://example.org` covers every page of it."""
        return self.action("set_site_protection", origin=origin, enabled=enabled)

    def set_site_protection_many(self, items: list[SetSiteProtectionArgs]) -> list[dict[str, Any]]:
        """Runs `set_site_protection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_site_protection", items)

    def toggle_site_protection(self, *, origin: str) -> None:
        """Turns tracker protection off for this site, or back on, as the page's shield does."""
        return self.action("toggle_site_protection", origin=origin)

    def toggle_site_protection_many(self, items: list[ToggleSiteProtectionArgs]) -> list[dict[str, Any]]:
        """Runs `toggle_site_protection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("toggle_site_protection", items)

    def forget_site_forms(self, *, origin: str) -> None:
        """Forgets every form a site asked."""
        return self.action("forget_site_forms", origin=origin)

    def forget_site_forms_many(self, items: list[ForgetSiteFormsArgs]) -> list[dict[str, Any]]:
        """Runs `forget_site_forms` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("forget_site_forms", items)

    def forget_all_forms(self) -> None:
        """Forgets every form every site asked."""
        return self.action("forget_all_forms")

    def forget_all_forms_many(self, items: list[ForgetAllFormsArgs]) -> list[dict[str, Any]]:
        """Runs `forget_all_forms` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("forget_all_forms", items)

    def remember_form(self,
        *,
        origin: str,
        signature: str,
        label: str,
        page: str,
        questions: list[str],
        whole: bool,
    ) -> None:
        """Remembers what a page's forms ask, as its window reads them: the shape of each form and of its questions, never what a field held. A form read here already is read again: what it asks now replaces what it asked before. `questions` is one question a line, its fields a tab apart: name, the kind of answer, the label, `true` when it must be answered, and the answers it offers, a `|` between them."""
        return self.action("remember_form", origin=origin, signature=signature, label=label, page=page, questions=questions, whole=whole)

    def remember_form_many(self, items: list[RememberFormArgs]) -> list[dict[str, Any]]:
        """Runs `remember_form` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remember_form", items)

    def add_address(self) -> Address:
        """Adds an address for the forms the person fills."""
        return self.action("add_address")

    def clear_history(self) -> None:
        """Clears the browsing history. Bookmarks and remembered forms stay."""
        return self.action("clear_history")

    def clear_history_many(self, items: list[ClearHistoryArgs]) -> list[dict[str, Any]]:
        """Runs `clear_history` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_history", items)

    def visit(self,
        *,
        url: str,
        title: str,
        at: str,
    ) -> None:
        """Adds a page a window opened to the person's history, counting it when it is there already. A page that opens itself is a visit like any other."""
        return self.action("visit", url=url, title=title, at=at)

    def visit_many(self, items: list[VisitArgs]) -> list[dict[str, Any]]:
        """Runs `visit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("visit", items)

    def trim_history(self, *, most: int) -> None:
        """Keeps the person's own visits to their newest `most`, forgetting the oldest: the history stays within what a person could read, as every other list the system keeps does."""
        return self.action("trim_history", most=most)

    def trim_history_many(self, items: list[TrimHistoryArgs]) -> list[dict[str, Any]]:
        """Runs `trim_history` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("trim_history", items)

    def arrived_at(self, *, url: str) -> None:
        """Takes in a page a window arrived at: it joins the person's history."""
        return self.action("arrived_at", url=url)

    def arrived_at_many(self, items: list[ArrivedAtArgs]) -> list[dict[str, Any]]:
        """Runs `arrived_at` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("arrived_at", items)

    def import_browsing(self, *, file: Any) -> None:
        """Takes in the browsing data another browser holds, from the bundle this app's own import and `tilo migrate` hand over: its history, its bookmarks, or both."""
        return self.action("import_browsing", file=file)

    def import_browsing_many(self, items: list[ImportBrowsingArgs]) -> list[dict[str, Any]]:
        """Runs `import_browsing` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import_browsing", items)

    def import_history(self, *, bundle: str) -> None:
        """Imports the pages another browser's history holds, from the bundle this app's own import and `tilo migrate` hand over: a JSON text of `{'browser': '...', 'visits': [{'url': '...', 'title': '...', 'last': 1760000000, 'count': 1}]}`. Importing the same bundle twice keeps one set of pages."""
        return self.action("import_history", bundle=bundle)

    def import_history_many(self, items: list[ImportHistoryArgs]) -> list[dict[str, Any]]:
        """Runs `import_history` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import_history", items)

    def import_bookmarks(self, *, bundle: str) -> None:
        """Imports the bookmarks another browser holds, from the bundle this app's own import hands over: the `ImportBundle` JSON with bookmark chunks (`{'bookmarks': [{'url': '...', 'title': '...', 'folder': 'Work'}]}`). Importing the same file twice keeps one bookmark of each page."""
        return self.action("import_bookmarks", bundle=bundle)

    def import_bookmarks_many(self, items: list[ImportBookmarksArgs]) -> list[dict[str, Any]]:
        """Runs `import_bookmarks` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import_bookmarks", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_page", "new_private_page", "go", "bookmark", "bookmark_many", "set_site_protection", "set_site_protection_many", "toggle_site_protection", "toggle_site_protection_many", "forget_site_forms", "forget_site_forms_many", "forget_all_forms", "forget_all_forms_many", "remember_form", "remember_form_many", "add_address", "clear_history", "clear_history_many", "visit", "visit_many", "trim_history", "trim_history_many", "arrived_at", "arrived_at_many", "import_browsing", "import_browsing_many", "import_history", "import_history_many", "import_bookmarks", "import_bookmarks_many"})


class TiloBrowser(Resource):
    """Browser — `tl.so.tilo.browser`."""
    __surface__ = "so.tilo.browser@172587f7789744b0"
    def open_download(self,
        *,
        name: str,
        path: str,
    ) -> None:
        """Opens a saved download in the app that opens its kind, through the system's one open."""
        return self.action("open_download", name=name, path=path)

    def reveal_download(self, *, path: str) -> None:
        """Shows where a saved download stands in Files, with the file marked: the system's one place for what a window saved."""
        return self.action("reveal_download", path=path)

    def reveal_download_many(self, items: list[RevealDownloadArgs]) -> list[dict[str, Any]]:
        """Runs `reveal_download` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("reveal_download", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"open_download", "reveal_download", "reveal_download_many"})


