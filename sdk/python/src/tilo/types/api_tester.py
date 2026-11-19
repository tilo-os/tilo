"""Types for API Tester (`so.tilo.api-tester`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Workspace` is the app's `api-tester.workspace` at `api-tester/<resource>`, read with
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
    "AuthScheme",
    "BodyMode",
    "Credentials",
    "Environment",
    "HttpMethod",
    "RequestResource",
    "Row",
    "SavedCollection",
    "Sent",
    "Variable",
    "Workspace",
    "NewRequestArgs",
    "NewCollectionArgs",
    "DeleteCollectionArgs",
    "AddRequestArgs",
    "SaveRequestArgs",
    "OpenRequestArgs",
    "DeleteRequestArgs",
    "NewEnvironmentArgs",
    "DeleteEnvironmentArgs",
    "UseEnvironmentArgs",
    "AddVariableArgs",
    "OpenSentArgs",
    "SaveSentArgs",
    "DeleteSentArgs",
    "RecordSentArgs",
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
    variables: list[Entry[Variable]]

HttpMethod = Literal["get", "post", "put", "patch", "delete", "head", "options"]

class RequestResource(TypedDict):
    name: str
    method: HttpMethod
    url: str
    params: list[Entry[Row]]
    headers: list[Entry[Row]]
    auth: Credentials
    body_mode: BodyMode
    body: str
    form: list[Entry[Row]]

class Row(TypedDict):
    key: str
    value: str
    enabled: bool

class SavedCollection(TypedDict):
    name: str
    requests: list[Entry[RequestResource]]

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

NewRequestArgs = TypedDict("NewRequestArgs", {
})
"""The arguments of `new-request`."""

NewCollectionArgs = TypedDict("NewCollectionArgs", {
    "name": "str",
})
"""The arguments of `new-collection`."""

DeleteCollectionArgs = TypedDict("DeleteCollectionArgs", {
    "collection": "str",
})
"""The arguments of `delete-collection`."""

AddRequestArgs = TypedDict("AddRequestArgs", {
    "collection": "str",
    "name": "str",
})
"""The arguments of `add-request`."""

SaveRequestArgs = TypedDict("SaveRequestArgs", {
    "collection": "str",
    "name": NotRequired["str | None"],
})
"""The arguments of `save-request`."""

OpenRequestArgs = TypedDict("OpenRequestArgs", {
    "collection": "str",
    "request": "str",
})
"""The arguments of `open-request`."""

DeleteRequestArgs = TypedDict("DeleteRequestArgs", {
    "collection": "str",
    "request": "str",
})
"""The arguments of `delete-request`."""

NewEnvironmentArgs = TypedDict("NewEnvironmentArgs", {
    "name": "str",
})
"""The arguments of `new-environment`."""

DeleteEnvironmentArgs = TypedDict("DeleteEnvironmentArgs", {
    "environment": "str",
})
"""The arguments of `delete-environment`."""

UseEnvironmentArgs = TypedDict("UseEnvironmentArgs", {
    "environment": "str",
})
"""The arguments of `use-environment`."""

AddVariableArgs = TypedDict("AddVariableArgs", {
    "environment": "str",
    "key": "str",
    "value": NotRequired["str | None"],
    "secret": NotRequired["bool | None"],
})
"""The arguments of `add-variable`."""

OpenSentArgs = TypedDict("OpenSentArgs", {
    "sent": "str",
})
"""The arguments of `open-sent`."""

SaveSentArgs = TypedDict("SaveSentArgs", {
    "collection": "str",
    "sent": "str",
    "name": NotRequired["str | None"],
})
"""The arguments of `save-sent`."""

DeleteSentArgs = TypedDict("DeleteSentArgs", {
    "sent": "str",
})
"""The arguments of `delete-sent`."""

RecordSentArgs = TypedDict("RecordSentArgs", {
    "method": "str",
    "url": "str",
    "status": "int",
    "duration_ms": "int",
})
"""The arguments of `record-sent`."""

class Workspace(Resource):
    """API workspace — `api-tester.workspace`."""
    __surface__ = "so.tilo.api-tester@288c87f5245c03fd"
    collections: list[Entry[SavedCollection]]
    environments: list[Entry[Environment]]
    active_environment: str | None
    request: RequestResource
    @property
    def opened(self) -> str:
        """Read only."""
        return self.fields["opened"]
    @property
    def history(self) -> list[Entry[Sent]]:
        """Read only."""
        return self.fields["history"]

    def new_request(self) -> None:
        """Runs the app's action."""
        return self.action("new-request")

    def new_request_many(self, items: list[NewRequestArgs]) -> list[dict[str, Any]]:
        """Runs `new-request` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("new-request", items)

    def new_collection(self, *, name: str) -> None:
        """Runs the app's action."""
        return self.action("new-collection", name=name)

    def new_collection_many(self, items: list[NewCollectionArgs]) -> list[dict[str, Any]]:
        """Runs `new-collection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("new-collection", items)

    def delete_collection(self, *, collection: str) -> None:
        """Runs the app's action."""
        return self.action("delete-collection", collection=collection)

    def delete_collection_many(self, items: list[DeleteCollectionArgs]) -> list[dict[str, Any]]:
        """Runs `delete-collection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-collection", items)

    def add_request(self,
        *,
        collection: str,
        name: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("add-request", collection=collection, name=name)

    def add_request_many(self, items: list[AddRequestArgs]) -> list[dict[str, Any]]:
        """Runs `add-request` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-request", items)

    def save_request(self,
        *,
        collection: str,
        name: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["collection"] = collection
        if name is not None:
            args["name"] = name
        return self.action("save-request", **args)

    def save_request_many(self, items: list[SaveRequestArgs]) -> list[dict[str, Any]]:
        """Runs `save-request` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("save-request", items)

    def open_request(self,
        *,
        collection: str,
        request: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("open-request", collection=collection, request=request)

    def open_request_many(self, items: list[OpenRequestArgs]) -> list[dict[str, Any]]:
        """Runs `open-request` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("open-request", items)

    def delete_request(self,
        *,
        collection: str,
        request: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("delete-request", collection=collection, request=request)

    def delete_request_many(self, items: list[DeleteRequestArgs]) -> list[dict[str, Any]]:
        """Runs `delete-request` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-request", items)

    def new_environment(self, *, name: str) -> None:
        """Runs the app's action."""
        return self.action("new-environment", name=name)

    def new_environment_many(self, items: list[NewEnvironmentArgs]) -> list[dict[str, Any]]:
        """Runs `new-environment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("new-environment", items)

    def delete_environment(self, *, environment: str) -> None:
        """Runs the app's action."""
        return self.action("delete-environment", environment=environment)

    def delete_environment_many(self, items: list[DeleteEnvironmentArgs]) -> list[dict[str, Any]]:
        """Runs `delete-environment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-environment", items)

    def use_environment(self, *, environment: str) -> None:
        """Runs the app's action."""
        return self.action("use-environment", environment=environment)

    def use_environment_many(self, items: list[UseEnvironmentArgs]) -> list[dict[str, Any]]:
        """Runs `use-environment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("use-environment", items)

    def add_variable(self,
        *,
        environment: str,
        key: str,
        value: str | None = None,
        secret: bool | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["environment"] = environment
        args["key"] = key
        if value is not None:
            args["value"] = value
        if secret is not None:
            args["secret"] = secret
        return self.action("add-variable", **args)

    def add_variable_many(self, items: list[AddVariableArgs]) -> list[dict[str, Any]]:
        """Runs `add-variable` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add-variable", items)

    def open_sent(self, *, sent: str) -> None:
        """Runs the app's action."""
        return self.action("open-sent", sent=sent)

    def open_sent_many(self, items: list[OpenSentArgs]) -> list[dict[str, Any]]:
        """Runs `open-sent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("open-sent", items)

    def save_sent(self,
        *,
        collection: str,
        sent: str,
        name: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["collection"] = collection
        args["sent"] = sent
        if name is not None:
            args["name"] = name
        return self.action("save-sent", **args)

    def save_sent_many(self, items: list[SaveSentArgs]) -> list[dict[str, Any]]:
        """Runs `save-sent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("save-sent", items)

    def delete_sent(self, *, sent: str) -> None:
        """Runs the app's action."""
        return self.action("delete-sent", sent=sent)

    def delete_sent_many(self, items: list[DeleteSentArgs]) -> list[dict[str, Any]]:
        """Runs `delete-sent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-sent", items)

    def record_sent(self,
        *,
        method: str,
        url: str,
        status: int,
        duration_ms: int,
    ) -> None:
        """Runs the app's action."""
        return self.action("record-sent", method=method, url=url, status=status, duration_ms=duration_ms)

    def record_sent_many(self, items: list[RecordSentArgs]) -> list[dict[str, Any]]:
        """Runs `record-sent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("record-sent", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_request", "new_request_many", "new_collection", "new_collection_many", "delete_collection", "delete_collection_many", "add_request", "add_request_many", "save_request", "save_request_many", "open_request", "open_request_many", "delete_request", "delete_request_many", "new_environment", "new_environment_many", "delete_environment", "delete_environment_many", "use_environment", "use_environment_many", "add_variable", "add_variable_many", "open_sent", "open_sent_many", "save_sent", "save_sent_many", "delete_sent", "delete_sent_many", "record_sent", "record_sent_many"})


