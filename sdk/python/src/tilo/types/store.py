"""Types for App Store (`so.tilo.store`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Catalog` is the app's `store.catalog` at `store/<resource>`, read with
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
    "Report",
    "Catalog",
]

class Report(TypedDict):
    status: str
    note: str
    case: str | None

class Catalog(Resource):
    """App Store — `store.catalog`."""
    __surface__ = "so.tilo.store@e40586bf33717a0d"
    reports: dict[str, Report]

    def draft_report(self, *, app_id: str) -> Run | None:
        """Runs the app's action."""
        return self.action("draft-report", app_id=app_id)

    def record_moderation_case(self,
        *,
        app_id: str,
        case_id: str,
        status: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("record-moderation-case", app_id=app_id, case_id=case_id, status=status)

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

    def browse_remote(self, *, remote: str) -> None:
        """Runs the app's action."""
        return self.action("browse-remote", remote=remote)

    def set_category(self, *, category: str) -> None:
        """Runs the app's action."""
        return self.action("set-category", category=category)

    def clear_filters(self) -> None:
        """Runs the app's action."""
        return self.action("clear-filters")

    def show_details(self, *, app: str) -> None:
        """Runs the app's action."""
        return self.action("show-details", app=app)

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

    def show_remotes(self) -> None:
        """Runs the app's action."""
        return self.action("show-remotes")

    def show_trust(self) -> None:
        """Runs the app's action."""
        return self.action("show-trust")

    def prepare_moderation(self,
        *,
        app: str,
        appeal: bool,
    ) -> None:
        """Runs the app's action."""
        return self.action("prepare-moderation", app=app, appeal=appeal)


