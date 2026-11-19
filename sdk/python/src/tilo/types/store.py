"""Types for App Store (`so.tilo.store`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Catalog` is the app's `store.catalog` at `store/<resource>`, read with
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
    "Report",
    "Catalog",
    "DraftReportArgs",
    "RecordModerationCaseArgs",
]

class Report(TypedDict):
    status: str
    note: str
    case: str | None

DraftReportArgs = TypedDict("DraftReportArgs", {
    "app_id": "str",
})
"""The arguments of `draft-report`."""

RecordModerationCaseArgs = TypedDict("RecordModerationCaseArgs", {
    "app_id": "str",
    "case_id": "str",
    "status": "str",
})
"""The arguments of `record-moderation-case`."""

class Catalog(Resource):
    """App Store — `store.catalog`."""
    __surface__ = "so.tilo.store@9c1517d70d1f8d2a"
    reports: dict[str, Report]

    def draft_report(self, *, app_id: str) -> None:
        """Runs the app's action."""
        return self.action("draft-report", app_id=app_id)

    def draft_report_many(self, items: list[DraftReportArgs]) -> list[dict[str, Any]]:
        """Runs `draft-report` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("draft-report", items)

    def record_moderation_case(self,
        *,
        app_id: str,
        case_id: str,
        status: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("record-moderation-case", app_id=app_id, case_id=case_id, status=status)

    def record_moderation_case_many(self, items: list[RecordModerationCaseArgs]) -> list[dict[str, Any]]:
        """Runs `record-moderation-case` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("record-moderation-case", items)

    def inspect_catalogue(self,
        *,
        app: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if app is not None:
            args["app"] = app
        return self.action("inspect-catalogue", **args)

    def set_search(self, *, query: str) -> None:
        """Runs the app's action."""
        return self.action("set-search", query=query)

    def show_view(self, *, view: str) -> None:
        """Runs the app's action."""
        return self.action("show-view", view=view)

    def show_more(self) -> None:
        """Runs the app's action."""
        return self.action("show-more")

    def set_category(self, *, category: str) -> None:
        """Runs the app's action."""
        return self.action("set-category", category=category)

    def clear_filters(self) -> None:
        """Runs the app's action."""
        return self.action("clear-filters")

    def show_runtime_details(self,
        *,
        runtime: str,
        registry: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["runtime"] = runtime
        if registry is not None:
            args["registry"] = registry
        return self.action("show-runtime-details", **args)

    def set_technical_details(self, *, expanded: bool) -> None:
        """Runs the app's action."""
        return self.action("set-technical-details", expanded=expanded)

    def open_reference(self,
        *,
        path: Any | None = None,
        contents: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if path is not None:
            args["path"] = path
        if contents is not None:
            args["contents"] = contents
        return self.action("open-reference", **args)

    def show_catalogue(self) -> None:
        """Runs the app's action."""
        return self.action("show-catalogue")

    def prepare_moderation(self,
        *,
        app: str,
        appeal: bool,
    ) -> None:
        """Runs the app's action."""
        return self.action("prepare-moderation", app=app, appeal=appeal)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"draft_report", "draft_report_many", "record_moderation_case", "record_moderation_case_many", "inspect_catalogue", "set_search", "show_view", "show_more", "set_category", "clear_filters", "show_runtime_details", "set_technical_details", "open_reference", "show_catalogue", "prepare_moderation"})


