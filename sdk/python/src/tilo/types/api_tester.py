"""Types for API Tester (`so.tilo.api-tester`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Workspace` is the app's `api-tester.workspace` at `api-tester/<resource>`, read with
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
    "AuthScheme",
    "BodyMode",
    "Credentials",
    "Environment",
    "HttpMethod",
    "RequestRecord",
    "Row",
    "SavedCollection",
    "Sent",
    "Variable",
    "Workspace",
]

AuthScheme = Literal["none", "bearer", "basic", "api_key"]

BodyMode = Literal["none", "json", "form", "raw"]

class Credentials(TypedDict):
    scheme: AuthScheme
    username: str
    header: str
    reference: str

class Environment(TypedDict):
    name: str
    variables: list[Variable]

HttpMethod = Literal["get", "post", "put", "patch", "delete", "head", "options"]

class RequestRecord(TypedDict):
    name: str
    method: HttpMethod
    url: str
    params: list[Row]
    headers: list[Row]
    auth: Credentials
    body_mode: BodyMode
    body: str
    form: list[Row]

class Row(TypedDict):
    key: str
    value: str
    enabled: bool

class SavedCollection(TypedDict):
    name: str
    requests: list[RequestRecord]

class Sent(TypedDict):
    route: str
    method: HttpMethod
    url: str
    status: int
    duration_ms: int
    at_ms: int
    replay: str

class Variable(TypedDict):
    key: str
    value: str
    enabled: bool
    secret: bool

class Workspace(Resource):
    """API workspace — `api-tester.workspace`."""
    __surface__ = "so.tilo.api-tester@2dc7adfdb370ac66"
    collections: list[SavedCollection]
    environments: list[Environment]
    active_environment: str | None
    request: RequestRecord
    @property
    def opened(self) -> str:
        """Read only."""
        return self.fields["opened"]
    @property
    def history(self) -> list[Sent]:
        """Read only."""
        return self.fields["history"]

    def import_workspace(self, *, workspace: Any) -> Run | None:
        """Runs the app's action."""
        return self.action("import-workspace", workspace=workspace)

    def new_request(self) -> Run | None:
        """Runs the app's action."""
        return self.action("new-request")

    def new_collection(self, *, name: str) -> Run | None:
        """Runs the app's action."""
        return self.action("new-collection", name=name)

    def delete_collection(self, *, collection: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-collection", collection=collection)

    def add_request(self,
        *,
        collection: str,
        name: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("add-request", collection=collection, name=name)

    def save_request(self,
        *,
        collection: str,
        name: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["collection"] = collection
        if name is not None:
            args["name"] = name
        return self.action("save-request", **args)

    def open_request(self,
        *,
        collection: str,
        request: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("open-request", collection=collection, request=request)

    def delete_request(self,
        *,
        collection: str,
        request: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-request", collection=collection, request=request)

    def new_environment(self, *, name: str) -> Run | None:
        """Runs the app's action."""
        return self.action("new-environment", name=name)

    def delete_environment(self, *, environment: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-environment", environment=environment)

    def use_environment(self, *, environment: str) -> Run | None:
        """Runs the app's action."""
        return self.action("use-environment", environment=environment)

    def add_variable(self,
        *,
        environment: str,
        key: str,
        value: str | None = None,
        secret: bool | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["environment"] = environment
        args["key"] = key
        if value is not None:
            args["value"] = value
        if secret is not None:
            args["secret"] = secret
        return self.action("add-variable", **args)

    def open_sent(self, *, sent: str) -> Run | None:
        """Runs the app's action."""
        return self.action("open-sent", sent=sent)

    def save_sent(self,
        *,
        collection: str,
        sent: str,
        name: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["collection"] = collection
        args["sent"] = sent
        if name is not None:
            args["name"] = name
        return self.action("save-sent", **args)

    def delete_sent(self, *, sent: str) -> Run | None:
        """Runs the app's action."""
        return self.action("delete-sent", sent=sent)

    def record_sent(self,
        *,
        method: str,
        url: str,
        status: int,
        duration_ms: int,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("record-sent", method=method, url=url, status=status, duration_ms=duration_ms)


