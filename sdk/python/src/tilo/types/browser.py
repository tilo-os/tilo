"""Types for Browser (`so.tilo.browser`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Application` is the app's `browser.application` at `browser/<resource>`, read with
`await ….load()`: its fields are the app's fields (read-only ones without a setter)
and its methods are its actions. A method answers what the app declares; an action
that takes longer answers a `Run`. A record is the dict the wire carries, and an
enum whose cases carry fields is an object whose tag the app's schema names. An
action whose name is one the class already has is run with `action("…")`, which
the file says where it is. An undoable action that answers nothing also has a
`<action>_many`, which applies it to every argument object of a list in a few
all-or-nothing steps, all of them one run.
"""

from __future__ import annotations

from typing import Any, Literal, NotRequired, TypedDict
from tilo import Entry, Resource, Run

__all__ = [
    "Bookmark",
    "RememberedForm",
    "RememberedQuestion",
    "Settings",
    "Added",
    "ImportedVisit",
    "Visit",
    "Answer",
    "PageAction",
    "Tab",
    "AddressProfile",
    "Application",
    "PersonalBookmarks",
    "BrowsingHistory",
    "PageRecord",
    "AddressBook",
    "AddBookmarkArgs",
    "RemoveBookmarkArgs",
    "SetSiteProtectionArgs",
    "RememberFormsArgs",
    "ForgetFormArgs",
    "ForgetSiteFormsArgs",
    "ForgetAllFormsArgs",
    "PersonalBookmarksAddBookmarkArgs",
    "PersonalBookmarksRemoveBookmarkArgs",
    "ImportBookmarksArgs",
    "RemoveImportedNodeArgs",
    "VisitArgs",
    "ImportHistoryBundleArgs",
    "NameVisitArgs",
    "ForgetArgs",
    "ClearHistoryArgs",
]

class Bookmark(TypedDict):
    url: str
    title: str

class RememberedForm(TypedDict):
    signature: str
    label: str
    pages: list[str]
    questions: list[RememberedQuestion]
    submit: list[str]
    follows: str
    grew_from: str
    seen: int

class RememberedQuestion(TypedDict):
    name: str
    kind: str
    choices: list[str]
    required: bool
    fallback: str

class Settings(TypedDict):
    adblock_enabled: bool
    clear_on_exit: bool
    search_engine: str
    homepage: str

class Added(TypedDict):
    url: str
    title: str

class ImportedVisit(TypedDict):
    url: str
    title: str
    last: int
    count: int

class Visit(TypedDict):
    title: str
    last: int
    count: int

class Answer(TypedDict):
    name: str
    value: str

class PageAction(TypedDict):
    seq: int
    time_ms: int
    actor: str
    device: str
    device_name: str
    device_unknown: bool
    how: str
    action: str
    ok: bool
    code: str
    message: str
    url: str
    to_url: str
    navigation: str
    frame: str
    role: str
    name: str
    nth: int
    secret: bool
    text: str | None
    values: list[str] | None
    answers: list[Answer]

class Tab(TypedDict):
    last: int
    dropped: int
    at: int
    device: str

class AddressProfile(TypedDict):
    id: str
    label: str
    full_name: str
    given_name: str
    family_name: str
    street: str
    city: str
    region: str
    postal_code: str
    country: str

AddBookmarkArgs = TypedDict("AddBookmarkArgs", {
    "url": "str",
    "title": NotRequired["str | None"],
})
"""The arguments of `add-bookmark`."""

RemoveBookmarkArgs = TypedDict("RemoveBookmarkArgs", {
    "bookmark": "str",
})
"""The arguments of `remove-bookmark`."""

SetSiteProtectionArgs = TypedDict("SetSiteProtectionArgs", {
    "origin": "str",
    "enabled": "bool",
})
"""The arguments of `set-site-protection`."""

RememberFormsArgs = TypedDict("RememberFormsArgs", {
    "origin": "str",
    "page": "str",
    "forms": "list[RememberedForm]",
    "whole": "bool",
    "at": "int",
})
"""The arguments of `remember-forms`."""

ForgetFormArgs = TypedDict("ForgetFormArgs", {
    "origin": "str",
    "signature": "str",
})
"""The arguments of `forget-form`."""

ForgetSiteFormsArgs = TypedDict("ForgetSiteFormsArgs", {
    "origin": "str",
})
"""The arguments of `forget-site-forms`."""

ForgetAllFormsArgs = TypedDict("ForgetAllFormsArgs", {
})
"""The arguments of `forget-all-forms`."""

PersonalBookmarksAddBookmarkArgs = TypedDict("PersonalBookmarksAddBookmarkArgs", {
    "url": "str",
    "title": "str",
})
"""The arguments of `add-bookmark`."""

PersonalBookmarksRemoveBookmarkArgs = TypedDict("PersonalBookmarksRemoveBookmarkArgs", {
    "url": "str",
    "observed_tags": "list[str]",
})
"""The arguments of `remove-bookmark`."""

ImportBookmarksArgs = TypedDict("ImportBookmarksArgs", {
    "bundle": "str",
})
"""The arguments of `import-bookmarks`."""

RemoveImportedNodeArgs = TypedDict("RemoveImportedNodeArgs", {
    "source": "str",
    "id": "str",
})
"""The arguments of `remove-imported-node`."""

VisitArgs = TypedDict("VisitArgs", {
    "url": "str",
    "title": "str",
    "at": "int",
})
"""The arguments of `visit`."""

ImportHistoryBundleArgs = TypedDict("ImportHistoryBundleArgs", {
    "bundle": "str",
})
"""The arguments of `import-history-bundle`."""

NameVisitArgs = TypedDict("NameVisitArgs", {
    "url": "str",
    "title": "str",
})
"""The arguments of `name-visit`."""

ForgetArgs = TypedDict("ForgetArgs", {
    "url": "str",
})
"""The arguments of `forget`."""

ClearHistoryArgs = TypedDict("ClearHistoryArgs", {
})
"""The arguments of `clear-history`."""

class Application(Resource):
    """Browser — `browser.application`."""
    __surface__ = "so.tilo.browser@118ee95647821b37"
    bookmarks: list[Entry[Bookmark]]
    preferences: Settings
    @property
    def site_exceptions(self) -> list[str]:
        """Read only."""
        return self.fields["site_exceptions"]
    @property
    def site_forms(self) -> dict[str, RememberedForm]:
        """Read only."""
        return self.fields["site_forms"]

    def add_bookmark(self,
        *,
        url: str,
        title: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["url"] = url
        if title is not None:
            args["title"] = title
        return self.action("add-bookmark", **args)

    def add_bookmark_many(self, items: list[AddBookmarkArgs]) -> list[dict[str, Any]]:
        """Runs `add-bookmark` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("add-bookmark", items)

    def remove_bookmark(self, *, bookmark: str) -> None:
        """Runs the app's action."""
        return self.action("remove-bookmark", bookmark=bookmark)

    def remove_bookmark_many(self, items: list[RemoveBookmarkArgs]) -> list[dict[str, Any]]:
        """Runs `remove-bookmark` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("remove-bookmark", items)

    def set_site_protection(self,
        *,
        origin: str,
        enabled: bool,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-site-protection", origin=origin, enabled=enabled)

    def set_site_protection_many(self, items: list[SetSiteProtectionArgs]) -> list[dict[str, Any]]:
        """Runs `set-site-protection` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("set-site-protection", items)

    def remember_forms(self,
        *,
        origin: str,
        page: str,
        forms: list[RememberedForm],
        whole: bool,
        at: int,
    ) -> None:
        """Runs the app's action."""
        return self.action("remember-forms", origin=origin, page=page, forms=forms, whole=whole, at=at)

    def remember_forms_many(self, items: list[RememberFormsArgs]) -> list[dict[str, Any]]:
        """Runs `remember-forms` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("remember-forms", items)

    def forget_form(self,
        *,
        origin: str,
        signature: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("forget-form", origin=origin, signature=signature)

    def forget_form_many(self, items: list[ForgetFormArgs]) -> list[dict[str, Any]]:
        """Runs `forget-form` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("forget-form", items)

    def forget_site_forms(self, *, origin: str) -> None:
        """Runs the app's action."""
        return self.action("forget-site-forms", origin=origin)

    def forget_site_forms_many(self, items: list[ForgetSiteFormsArgs]) -> list[dict[str, Any]]:
        """Runs `forget-site-forms` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("forget-site-forms", items)

    def forget_all_forms(self) -> None:
        """Runs the app's action."""
        return self.action("forget-all-forms")

    def forget_all_forms_many(self, items: list[ForgetAllFormsArgs]) -> list[dict[str, Any]]:
        """Runs `forget-all-forms` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("forget-all-forms", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"add_bookmark", "add_bookmark_many", "remove_bookmark", "remove_bookmark_many", "set_site_protection", "set_site_protection_many", "remember_forms", "remember_forms_many", "forget_form", "forget_form_many", "forget_site_forms", "forget_site_forms_many", "forget_all_forms", "forget_all_forms_many"})


class PersonalBookmarks(Resource):
    """Personal bookmarks — `browser.personal-bookmarks`."""
    __surface__ = "so.tilo.browser@118ee95647821b37"
    @property
    def added(self) -> dict[str, Added]:
        """Read only."""
        return self.fields["added"]
    @property
    def removed(self) -> list[str]:
        """Read only."""
        return self.fields["removed"]
    @property
    def receipts(self) -> dict[str, str]:
        """Read only."""
        return self.fields["receipts"]
    @property
    def trees(self) -> dict[str, Any]:
        """Read only."""
        return self.fields["trees"]
    @property
    def hidden(self) -> list[str]:
        """Read only."""
        return self.fields["hidden"]

    def add_bookmark(self,
        *,
        url: str,
        title: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("add-bookmark", url=url, title=title)

    def add_bookmark_many(self, items: list[PersonalBookmarksAddBookmarkArgs]) -> list[dict[str, Any]]:
        """Runs `add-bookmark` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("add-bookmark", items)

    def remove_bookmark(self,
        *,
        url: str,
        observed_tags: list[str],
    ) -> None:
        """Runs the app's action."""
        return self.action("remove-bookmark", url=url, observed_tags=observed_tags)

    def remove_bookmark_many(self, items: list[PersonalBookmarksRemoveBookmarkArgs]) -> list[dict[str, Any]]:
        """Runs `remove-bookmark` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("remove-bookmark", items)

    def import_bookmarks(self, *, bundle: str) -> None:
        """Runs the app's action."""
        return self.action("import-bookmarks", bundle=bundle)

    def import_bookmarks_many(self, items: list[ImportBookmarksArgs]) -> list[dict[str, Any]]:
        """Runs `import-bookmarks` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("import-bookmarks", items)

    def remove_imported_node(self,
        *,
        source: str,
        id: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("remove-imported-node", source=source, id=id)

    def remove_imported_node_many(self, items: list[RemoveImportedNodeArgs]) -> list[dict[str, Any]]:
        """Runs `remove-imported-node` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("remove-imported-node", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"add_bookmark", "add_bookmark_many", "remove_bookmark", "remove_bookmark_many", "import_bookmarks", "import_bookmarks_many", "remove_imported_node", "remove_imported_node_many"})


class BrowsingHistory(Resource):
    """Browsing history — `browser.browsing-history`."""
    __surface__ = "so.tilo.browser@118ee95647821b37"
    @property
    def visits(self) -> dict[str, Visit]:
        """Read only."""
        return self.fields["visits"]
    @property
    def imported(self) -> dict[str, ImportedVisit]:
        """Read only."""
        return self.fields["imported"]
    @property
    def import_receipts(self) -> dict[str, str]:
        """Read only."""
        return self.fields["import_receipts"]
    @property
    def forgotten_imports(self) -> list[str]:
        """Read only."""
        return self.fields["forgotten_imports"]

    def visit(self,
        *,
        url: str,
        title: str,
        at: int,
    ) -> None:
        """Runs the app's action."""
        return self.action("visit", url=url, title=title, at=at)

    def visit_many(self, items: list[VisitArgs]) -> list[dict[str, Any]]:
        """Runs `visit` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("visit", items)

    def import_history_bundle(self, *, bundle: str) -> None:
        """Runs the app's action."""
        return self.action("import-history-bundle", bundle=bundle)

    def import_history_bundle_many(self, items: list[ImportHistoryBundleArgs]) -> list[dict[str, Any]]:
        """Runs `import-history-bundle` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("import-history-bundle", items)

    def name_visit(self,
        *,
        url: str,
        title: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("name-visit", url=url, title=title)

    def name_visit_many(self, items: list[NameVisitArgs]) -> list[dict[str, Any]]:
        """Runs `name-visit` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("name-visit", items)

    def forget(self, *, url: str) -> None:
        """Runs the app's action."""
        return self.action("forget", url=url)

    def forget_many(self, items: list[ForgetArgs]) -> list[dict[str, Any]]:
        """Runs `forget` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("forget", items)

    def clear_history(self) -> None:
        """Runs the app's action."""
        return self.action("clear-history")

    def clear_history_many(self, items: list[ClearHistoryArgs]) -> list[dict[str, Any]]:
        """Runs `clear-history` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back."""
        return self.many("clear-history", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"visit", "visit_many", "import_history_bundle", "import_history_bundle_many", "name_visit", "name_visit_many", "forget", "forget_many", "clear_history", "clear_history_many"})


class PageRecord(Resource):
    """Page record — `browser.page-record`."""
    __surface__ = "so.tilo.browser@118ee95647821b37"
    @property
    def actions(self) -> dict[str, PageAction]:
        """Read only."""
        return self.fields["actions"]
    @property
    def tabs(self) -> dict[str, Tab]:
        """Read only."""
        return self.fields["tabs"]


class AddressBook(Resource):
    """Address book — `browser.address-book`."""
    __surface__ = "so.tilo.browser@118ee95647821b37"
    enabled: bool
    profiles: list[Entry[AddressProfile]]


