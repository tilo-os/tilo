"""Types for Browser (`so.tilo.browser`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Application` is the app's `browser.application` at `browser/<resource>`, read with
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

class Application(Resource):
    """Browser — `browser.application`."""
    bookmarks: list[Bookmark]
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
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["url"] = url
        if title is not None:
            args["title"] = title
        return self.action("add-bookmark", **args)

    def remove_bookmark(self, *, bookmark: str) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-bookmark", bookmark=bookmark)

    def set_site_protection(self,
        *,
        origin: str,
        enabled: bool,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-site-protection", origin=origin, enabled=enabled)

    def remember_forms(self,
        *,
        origin: str,
        page: str,
        forms: list[RememberedForm],
        whole: bool,
        at: int,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("remember-forms", origin=origin, page=page, forms=forms, whole=whole, at=at)

    def forget_form(self,
        *,
        origin: str,
        signature: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("forget-form", origin=origin, signature=signature)

    def forget_site_forms(self, *, origin: str) -> Run | None:
        """Runs the app's action."""
        return self.action("forget-site-forms", origin=origin)

    def forget_all_forms(self) -> Run | None:
        """Runs the app's action."""
        return self.action("forget-all-forms")


class PersonalBookmarks(Resource):
    """Personal bookmarks — `browser.personal-bookmarks`."""
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
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("add-bookmark", url=url, title=title)

    def remove_bookmark(self,
        *,
        url: str,
        observed_tags: list[str],
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-bookmark", url=url, observed_tags=observed_tags)

    def import_bookmarks(self, *, bundle: str) -> Run | None:
        """Runs the app's action."""
        return self.action("import-bookmarks", bundle=bundle)

    def remove_imported_node(self,
        *,
        source: str,
        id: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-imported-node", source=source, id=id)


class BrowsingHistory(Resource):
    """Browsing history — `browser.browsing-history`."""
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
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("visit", url=url, title=title, at=at)

    def import_history_bundle(self, *, bundle: str) -> Run | None:
        """Runs the app's action."""
        return self.action("import-history-bundle", bundle=bundle)

    def name_visit(self,
        *,
        url: str,
        title: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("name-visit", url=url, title=title)

    def forget(self, *, url: str) -> Run | None:
        """Runs the app's action."""
        return self.action("forget", url=url)

    def clear_history(self) -> Run | None:
        """Runs the app's action."""
        return self.action("clear-history")


class PageRecord(Resource):
    """Page record — `browser.page-record`."""
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
    enabled: bool
    profiles: list[AddressProfile]


