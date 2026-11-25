"""Types for API Tester (`so.tilo.api-tester`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `api-tester/<resource>`, read with
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
    "HttpMethod",
    "Notice",
    "Row",
    "Credentials",
    "Request",
    "Answer",
    "AnswerHeader",
    "Environment",
    "Variable",
    "Sent",
    "SavedCollection",
    "Workspace",
    "SendAsk",
    "KeepAsk",
    "ForgetAsk",
    "KeptCredential",
    "SendState",
    "Answered",
    "Replay",
    "ApiTester",
    "FillFromArgs",
    "AddParamArgs",
    "DeleteParamArgs",
    "AddHeaderArgs",
    "DeleteHeaderArgs",
    "AddFormFieldArgs",
    "DeleteFormFieldArgs",
    "RenameEnvironmentArgs",
    "DeleteVariableArgs",
    "KeepVariableArgs",
    "RenameCollectionArgs",
    "DeleteCollectionArgs",
    "OpenRequestArgs",
    "RenameDraftArgs",
    "DeleteRequestArgs",
    "DeleteOpenedRequestArgs",
    "DeleteEnvironmentArgs",
    "DeleteActiveEnvironmentArgs",
    "UseEnvironmentArgs",
    "NoEnvironmentArgs",
    "KeepCredentialArgs",
    "SetSchemeArgs",
    "ForgetCredentialArgs",
    "StartSendArgs",
    "SendFailedArgs",
    "CancelSendArgs",
    "PollSendArgs",
    "OpenSentArgs",
    "DeleteSentArgs",
    "DismissNoticeArgs",
    "KeepAnswerArgs",
    "RecordSentArgs",
    "SetBodyModeArgs",
]

AuthScheme = Literal["none", "bearer", "basic", "api_key"]
"""How a request signs in."""

BodyMode = Literal["none", "json", "form", "raw"]
"""What a request's body is."""

HttpMethod = Literal["get", "post", "put", "patch", "delete", "head", "options"]
"""The methods a request is sent with, in the order the chooser lists them."""

FillFromArgs = TypedDict("FillFromArgs", {
    "src": "Request",
})
"""The arguments of `fill_from`."""

AddParamArgs = TypedDict("AddParamArgs", {
})
"""The arguments of `add_param`."""

DeleteParamArgs = TypedDict("DeleteParamArgs", {
    "entry": "Row",
})
"""The arguments of `delete_param`."""

AddHeaderArgs = TypedDict("AddHeaderArgs", {
})
"""The arguments of `add_header`."""

DeleteHeaderArgs = TypedDict("DeleteHeaderArgs", {
    "entry": "Row",
})
"""The arguments of `delete_header`."""

AddFormFieldArgs = TypedDict("AddFormFieldArgs", {
})
"""The arguments of `add_form_field`."""

DeleteFormFieldArgs = TypedDict("DeleteFormFieldArgs", {
    "entry": "Row",
})
"""The arguments of `delete_form_field`."""

RenameEnvironmentArgs = TypedDict("RenameEnvironmentArgs", {
    "name": "str",
})
"""The arguments of `rename_environment`."""

DeleteVariableArgs = TypedDict("DeleteVariableArgs", {
    "v": "Variable",
})
"""The arguments of `delete_variable`."""

KeepVariableArgs = TypedDict("KeepVariableArgs", {
    "value": "str",
})
"""The arguments of `keep_variable`."""

RenameCollectionArgs = TypedDict("RenameCollectionArgs", {
    "name": "str",
})
"""The arguments of `rename_collection`."""

DeleteCollectionArgs = TypedDict("DeleteCollectionArgs", {
    "c": "SavedCollection",
})
"""The arguments of `delete_collection`."""

OpenRequestArgs = TypedDict("OpenRequestArgs", {
    "r": "Request",
})
"""The arguments of `open_request`."""

RenameDraftArgs = TypedDict("RenameDraftArgs", {
    "name": "str",
})
"""The arguments of `rename_draft`."""

DeleteRequestArgs = TypedDict("DeleteRequestArgs", {
    "r": "Request",
})
"""The arguments of `delete_request`."""

DeleteOpenedRequestArgs = TypedDict("DeleteOpenedRequestArgs", {
})
"""The arguments of `delete_opened_request`."""

DeleteEnvironmentArgs = TypedDict("DeleteEnvironmentArgs", {
    "e": "Environment",
})
"""The arguments of `delete_environment`."""

DeleteActiveEnvironmentArgs = TypedDict("DeleteActiveEnvironmentArgs", {
})
"""The arguments of `delete_active_environment`."""

UseEnvironmentArgs = TypedDict("UseEnvironmentArgs", {
    "e": "Environment",
})
"""The arguments of `use_environment`."""

NoEnvironmentArgs = TypedDict("NoEnvironmentArgs", {
})
"""The arguments of `no_environment`."""

KeepCredentialArgs = TypedDict("KeepCredentialArgs", {
    "value": "str",
})
"""The arguments of `keep_credential`."""

SetSchemeArgs = TypedDict("SetSchemeArgs", {
    "scheme": "AuthScheme",
})
"""The arguments of `set_scheme`."""

ForgetCredentialArgs = TypedDict("ForgetCredentialArgs", {
})
"""The arguments of `forget_credential`."""

StartSendArgs = TypedDict("StartSendArgs", {
})
"""The arguments of `start_send`."""

SendFailedArgs = TypedDict("SendFailedArgs", {
    "why": "str",
})
"""The arguments of `send_failed`."""

CancelSendArgs = TypedDict("CancelSendArgs", {
})
"""The arguments of `cancel_send`."""

PollSendArgs = TypedDict("PollSendArgs", {
})
"""The arguments of `poll_send`."""

OpenSentArgs = TypedDict("OpenSentArgs", {
    "s": "Sent",
})
"""The arguments of `open_sent`."""

DeleteSentArgs = TypedDict("DeleteSentArgs", {
    "s": "Sent",
})
"""The arguments of `delete_sent`."""

DismissNoticeArgs = TypedDict("DismissNoticeArgs", {
})
"""The arguments of `dismiss_notice`."""

KeepAnswerArgs = TypedDict("KeepAnswerArgs", {
    "answer": "str",
})
"""The arguments of `keep_answer`."""

RecordSentArgs = TypedDict("RecordSentArgs", {
    "answer": "str",
})
"""The arguments of `record_sent`."""

SetBodyModeArgs = TypedDict("SetBodyModeArgs", {
    "mode": "BodyMode",
})
"""The arguments of `set_body_mode`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.api-tester@49fd293a1113805a"
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


class Row(Resource):
    """Row — `tl.so.tilo.api-tester.Row`."""
    __surface__ = "so.tilo.api-tester@49fd293a1113805a"
    gmr3auev: str  # key
    @property
    def _5i27urp3(self) -> str:
        """value (`5i27urp3` on the wire)."""
        return self.fields["5i27urp3"]

    @_5i27urp3.setter
    def _5i27urp3(self, value: str) -> None:
        self.edit("5i27urp3", value=value)
    vgdlc7cu: bool  # enabled


class Credentials(Resource):
    """Credentials — `tl.so.tilo.api-tester.Credentials`."""
    __surface__ = "so.tilo.api-tester@49fd293a1113805a"
    wu5jqin3: AuthScheme  # scheme
    eimiw54w: str  # username
    bq35bust: str  # header
    doie7hps: str  # reference


class Request(Resource):
    """Request — `tl.so.tilo.api-tester.Request`."""
    __surface__ = "so.tilo.api-tester@49fd293a1113805a"
    zerzgb7o: str  # name
    plszv7l4: HttpMethod  # method
    @property
    def _2myyqmpp(self) -> str:
        """url (`2myyqmpp` on the wire)."""
        return self.fields["2myyqmpp"]

    @_2myyqmpp.setter
    def _2myyqmpp(self, value: str) -> None:
        self.edit("2myyqmpp", value=value)
    d26dsrkb: list[Entry[Row]]  # params
    sp2vfioq: list[Entry[Row]]  # headers
    oubruxyi: Credentials  # auth
    @property
    def _26dsckeq(self) -> BodyMode:
        """body_mode (`26dsckeq` on the wire)."""
        return self.fields["26dsckeq"]

    @_26dsckeq.setter
    def _26dsckeq(self, value: BodyMode) -> None:
        self.edit("26dsckeq", value=value)
    ejporjru: str  # body
    @property
    def _2koo3omi(self) -> list[Entry[Row]]:
        """form (`2koo3omi` on the wire)."""
        return self.fields["2koo3omi"]

    @_2koo3omi.setter
    def _2koo3omi(self, value: list[Entry[Row]]) -> None:
        self.edit("2koo3omi", value=value)

    def fill_from(self, *, src: Request) -> None:
        """Copies everything a request holds into another one."""
        return self.action("fill_from", src=src)

    def fill_from_many(self, items: list[FillFromArgs]) -> list[dict[str, Any]]:
        """Runs `fill_from` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("fill_from", items)

    def add_param(self) -> None:
        """Adds a query parameter to the request in the builder."""
        return self.action("add_param")

    def add_param_many(self, items: list[AddParamArgs]) -> list[dict[str, Any]]:
        """Runs `add_param` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_param", items)

    def delete_param(self, *, entry: Row) -> None:
        """Takes a query parameter off the request in the builder."""
        return self.action("delete_param", entry=entry)

    def delete_param_many(self, items: list[DeleteParamArgs]) -> list[dict[str, Any]]:
        """Runs `delete_param` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_param", items)

    def add_header(self) -> None:
        """Adds a header to the request in the builder."""
        return self.action("add_header")

    def add_header_many(self, items: list[AddHeaderArgs]) -> list[dict[str, Any]]:
        """Runs `add_header` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_header", items)

    def delete_header(self, *, entry: Row) -> None:
        """Takes a header off the request in the builder."""
        return self.action("delete_header", entry=entry)

    def delete_header_many(self, items: list[DeleteHeaderArgs]) -> list[dict[str, Any]]:
        """Runs `delete_header` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_header", items)

    def add_form_field(self) -> None:
        """Adds a form field to the request in the builder."""
        return self.action("add_form_field")

    def add_form_field_many(self, items: list[AddFormFieldArgs]) -> list[dict[str, Any]]:
        """Runs `add_form_field` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_form_field", items)

    def delete_form_field(self, *, entry: Row) -> None:
        """Takes a form field off the request in the builder."""
        return self.action("delete_form_field", entry=entry)

    def delete_form_field_many(self, items: list[DeleteFormFieldArgs]) -> list[dict[str, Any]]:
        """Runs `delete_form_field` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_form_field", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"fill_from", "fill_from_many", "add_param", "add_param_many", "delete_param", "delete_param_many", "add_header", "add_header_many", "delete_header", "delete_header_many", "add_form_field", "add_form_field_many", "delete_form_field", "delete_form_field_many"})


class Answer(Resource):
    """Answer — `tl.so.tilo.api-tester.Answer`."""
    __surface__ = "so.tilo.api-tester@49fd293a1113805a"
    cmtsv52e: int  # status
    s2rpes4w: str  # status_text
    bmbte2qq: int  # duration_ms
    @property
    def _7xxkbrmd(self) -> int:
        """size_bytes (`7xxkbrmd` on the wire)."""
        return self.fields["7xxkbrmd"]

    @_7xxkbrmd.setter
    def _7xxkbrmd(self, value: int) -> None:
        self.edit("7xxkbrmd", value=value)
    lfculcqs: bool  # size_exact
    @property
    def _52brcezl(self) -> int:
        """shown_bytes (`52brcezl` on the wire)."""
        return self.fields["52brcezl"]

    @_52brcezl.setter
    def _52brcezl(self, value: int) -> None:
        self.edit("52brcezl", value=value)
    dczl7ahz: bool  # binary
    ue25vzen: str  # content_type
    nxs5lmpc: str  # body
    ryqihqco: str  # pretty
    anvyklqi: list[Entry[AnswerHeader]]  # headers


class AnswerHeader(Resource):
    """AnswerHeader — `tl.so.tilo.api-tester.AnswerHeader`."""
    __surface__ = "so.tilo.api-tester@49fd293a1113805a"
    q5ydbtzd: str  # name
    vyjpofsi: str  # value


class Environment(Resource):
    """Environment — `tl.so.tilo.api-tester.Environment`."""
    __surface__ = "so.tilo.api-tester@49fd293a1113805a"
    @property
    def _2kunn2pf(self) -> str:
        """name (`2kunn2pf` on the wire)."""
        return self.fields["2kunn2pf"]

    @_2kunn2pf.setter
    def _2kunn2pf(self, value: str) -> None:
        self.edit("2kunn2pf", value=value)
    yhhzdhos: list[Entry[Variable]]  # variables

    def rename_environment(self, *, name: str) -> None:
        """Renames an environment."""
        return self.action("rename_environment", name=name)

    def rename_environment_many(self, items: list[RenameEnvironmentArgs]) -> list[dict[str, Any]]:
        """Runs `rename_environment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename_environment", items)

    def add_variable(self,
        *,
        key: str,
        secret: bool | None = None,
    ) -> Variable:
        """Adds a variable to an environment. A secret's value is typed after it stands."""
        args: dict[str, Any] = {}
        args["key"] = key
        if secret is not None:
            args["secret"] = secret
        return self.action("add_variable", **args)

    def delete_variable(self, *, v: Variable) -> None:
        """Takes a variable off its environment, and its value off this device."""
        return self.action("delete_variable", v=v)

    def delete_variable_many(self, items: list[DeleteVariableArgs]) -> list[dict[str, Any]]:
        """Runs `delete_variable` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_variable", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"rename_environment", "rename_environment_many", "add_variable", "delete_variable", "delete_variable_many"})


class Variable(Resource):
    """Variable — `tl.so.tilo.api-tester.Variable`."""
    __surface__ = "so.tilo.api-tester@49fd293a1113805a"
    i3e2wno7: str  # key
    qqgvzyfe: str  # value
    @property
    def _7bxonldr(self) -> bool:
        """enabled (`7bxonldr` on the wire)."""
        return self.fields["7bxonldr"]

    @_7bxonldr.setter
    def _7bxonldr(self, value: bool) -> None:
        self.edit("7bxonldr", value=value)
    og7vyhyy: bool  # secret
    v6jf7adc: str  # reference

    def keep_variable(self, *, value: str) -> None:
        """Keeps the value typed for a secret variable where the workspace cannot read it."""
        return self.action("keep_variable", value=value)

    def keep_variable_many(self, items: list[KeepVariableArgs]) -> list[dict[str, Any]]:
        """Runs `keep_variable` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("keep_variable", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"keep_variable", "keep_variable_many"})


class Sent(Resource):
    """Sent — `tl.so.tilo.api-tester.Sent`."""
    __surface__ = "so.tilo.api-tester@49fd293a1113805a"
    ikluhyoi: str  # route
    fwwi364f: HttpMethod  # method
    w7aoohfv: str  # url
    @property
    def _7pfct73j(self) -> int:
        """status (`7pfct73j` on the wire)."""
        return self.fields["7pfct73j"]

    @_7pfct73j.setter
    def _7pfct73j(self, value: int) -> None:
        self.edit("7pfct73j", value=value)
    cno6gxwc: int  # duration_ms
    irpsjk53: str  # at
    @property
    def _4mnpilrd(self) -> str:
        """replay (`4mnpilrd` on the wire)."""
        return self.fields["4mnpilrd"]

    @_4mnpilrd.setter
    def _4mnpilrd(self, value: str) -> None:
        self.edit("4mnpilrd", value=value)


class SavedCollection(Resource):
    """SavedCollection — `tl.so.tilo.api-tester.SavedCollection`."""
    __surface__ = "so.tilo.api-tester@49fd293a1113805a"
    @property
    def _6bygepf4(self) -> str:
        """name (`6bygepf4` on the wire)."""
        return self.fields["6bygepf4"]

    @_6bygepf4.setter
    def _6bygepf4(self, value: str) -> None:
        self.edit("6bygepf4", value=value)
    vghsgjnl: list[Entry[Request]]  # requests

    def rename_collection(self, *, name: str) -> None:
        """Renames a collection."""
        return self.action("rename_collection", name=name)

    def rename_collection_many(self, items: list[RenameCollectionArgs]) -> list[dict[str, Any]]:
        """Runs `rename_collection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename_collection", items)

    def add_request(self,
        *,
        name: str | None = None,
    ) -> Request:
        """Adds a request to a collection."""
        args: dict[str, Any] = {}
        if name is not None:
            args["name"] = name
        return self.action("add_request", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"rename_collection", "rename_collection_many", "add_request"})


class Workspace(Resource):
    """Workspace — `tl.so.tilo.api-tester.Workspace`."""
    __surface__ = "so.tilo.api-tester@49fd293a1113805a"
    bqk5mejh: list[Entry[SavedCollection]]  # collections
    @property
    def _7idvazp5(self) -> list[Entry[Environment]]:
        """environments (`7idvazp5` on the wire)."""
        return self.fields["7idvazp5"]

    @_7idvazp5.setter
    def _7idvazp5(self, value: list[Entry[Environment]]) -> None:
        self.edit("7idvazp5", value=value)
    bnuffmnw: Environment | None  # active_environment
    t7bbwzg4: Request  # builder
    oela3gry: Request | None  # opened
    yughiaxm: list[Entry[Sent]]  # history
    ylmn6amm: bool  # sending
    lzzl5iuz: str  # job
    be7aufil: str  # sent_replay
    @property
    def _6msktvdz(self) -> Answer:
        """answer (`6msktvdz` on the wire)."""
        return self.fields["6msktvdz"]

    @_6msktvdz.setter
    def _6msktvdz(self, value: Answer) -> None:
        self.edit("6msktvdz", value=value)
    bhgazpm3: bool  # answered
    @property
    def _674n5s6g(self) -> str:
        """failure (`674n5s6g` on the wire)."""
        return self.fields["674n5s6g"]

    @_674n5s6g.setter
    def _674n5s6g(self, value: str) -> None:
        self.edit("674n5s6g", value=value)
    si6y5pgr: str  # notice

    def new_request(self) -> Request:
        """Starts a blank request in the builder, leaving the one it held."""
        return self.action("new_request")

    def new_collection(self, *, name: str) -> SavedCollection:
        """Creates a collection to keep requests in."""
        return self.action("new_collection", name=name)

    def delete_collection(self, *, c: SavedCollection) -> None:
        """Deletes a collection and the requests saved in it."""
        return self.action("delete_collection", c=c)

    def delete_collection_many(self, items: list[DeleteCollectionArgs]) -> list[dict[str, Any]]:
        """Runs `delete_collection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_collection", items)

    def save_request(self, *, c: SavedCollection) -> Request:
        """Saves the request in the builder into a collection."""
        return self.action("save_request", c=c)

    def open_request(self, *, r: Request) -> None:
        """Opens a saved request in the builder."""
        return self.action("open_request", r=r)

    def open_request_many(self, items: list[OpenRequestArgs]) -> list[dict[str, Any]]:
        """Runs `open_request` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("open_request", items)

    def rename_draft(self, *, name: str) -> None:
        """Renames the request in the builder."""
        return self.action("rename_draft", name=name)

    def rename_draft_many(self, items: list[RenameDraftArgs]) -> list[dict[str, Any]]:
        """Runs `rename_draft` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename_draft", items)

    def delete_request(self, *, r: Request) -> None:
        """Deletes a saved request."""
        return self.action("delete_request", r=r)

    def delete_request_many(self, items: list[DeleteRequestArgs]) -> list[dict[str, Any]]:
        """Runs `delete_request` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_request", items)

    def delete_opened_request(self) -> None:
        """Deletes the saved request the builder holds a copy of."""
        return self.action("delete_opened_request")

    def delete_opened_request_many(self, items: list[DeleteOpenedRequestArgs]) -> list[dict[str, Any]]:
        """Runs `delete_opened_request` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_opened_request", items)

    def new_environment(self, *, name: str) -> Environment:
        """Creates an environment and starts using it."""
        return self.action("new_environment", name=name)

    def delete_environment(self, *, e: Environment) -> None:
        """Deletes an environment and the variables it held."""
        return self.action("delete_environment", e=e)

    def delete_environment_many(self, items: list[DeleteEnvironmentArgs]) -> list[dict[str, Any]]:
        """Runs `delete_environment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_environment", items)

    def delete_active_environment(self) -> None:
        """Deletes the environment in use."""
        return self.action("delete_active_environment")

    def delete_active_environment_many(self, items: list[DeleteActiveEnvironmentArgs]) -> list[dict[str, Any]]:
        """Runs `delete_active_environment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_active_environment", items)

    def use_environment(self, *, e: Environment) -> None:
        """Switches the environment whose variables fill what is sent."""
        return self.action("use_environment", e=e)

    def use_environment_many(self, items: list[UseEnvironmentArgs]) -> list[dict[str, Any]]:
        """Runs `use_environment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("use_environment", items)

    def no_environment(self) -> None:
        """Sends with no environment: every `{{variable}}` stays as typed."""
        return self.action("no_environment")

    def no_environment_many(self, items: list[NoEnvironmentArgs]) -> list[dict[str, Any]]:
        """Runs `no_environment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("no_environment", items)

    def keep_credential(self, *, value: str) -> None:
        """Keeps the value typed for the request's sign-in where the workspace cannot read it, and remembers only the reference."""
        return self.action("keep_credential", value=value)

    def keep_credential_many(self, items: list[KeepCredentialArgs]) -> list[dict[str, Any]]:
        """Runs `keep_credential` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("keep_credential", items)

    def set_scheme(self, *, scheme: AuthScheme) -> None:
        """Changes the scheme, taking the credential of the one before off this device."""
        return self.action("set_scheme", scheme=scheme)

    def set_scheme_many(self, items: list[SetSchemeArgs]) -> list[dict[str, Any]]:
        """Runs `set_scheme` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_scheme", items)

    def forget_credential(self) -> None:
        """Takes the request's sign-in off this device."""
        return self.action("forget_credential")

    def forget_credential_many(self, items: list[ForgetCredentialArgs]) -> list[dict[str, Any]]:
        """Runs `forget_credential` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("forget_credential", items)

    def send(self) -> str:
        """Sends what the builder holds to the address it names, in the environment in use, and answers the response the engine wrote: its status, status text, headers, size and body. An agent or a script calling `send` reads the answer in this one call instead of following a job. A person at the window sends with `start_send` instead, which starts the same request as a job they can stop while it waits: this action holds its caller until the answer arrives, so there is nothing to cancel, which is why the window never calls it for its Send control."""
        return self.action("send")

    def start_send(self) -> None:
        """Starts what `send` sends as a job the person at the window can stop, and leaves its answer to `trigger watch_send`. A person pressing Send must be able to stop waiting, which an action that waits for the answer cannot do; a caller that needs the answer in the call uses `send`."""
        return self.action("start_send")

    def start_send_many(self, items: list[StartSendArgs]) -> list[dict[str, Any]]:
        """Runs `start_send` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("start_send", items)

    def send_failed(self, *, why: str) -> None:
        """Ends a send that got no answer from the engine, saying why in the window."""
        return self.action("send_failed", why=why)

    def send_failed_many(self, items: list[SendFailedArgs]) -> list[dict[str, Any]]:
        """Runs `send_failed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("send_failed", items)

    def settle_send(self, *, state: str) -> str:
        """Folds what a send's job said of itself into the window, and answers the response the job brought when it came back with one."""
        return self.action("settle_send", state=state)

    def cancel_send(self) -> None:
        """Stops waiting for the service to answer."""
        return self.action("cancel_send")

    def cancel_send_many(self, items: list[CancelSendArgs]) -> list[dict[str, Any]]:
        """Runs `cancel_send` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("cancel_send", items)

    def poll_send(self) -> None:
        """Asks the device how the request on its way is doing, and keeps what it answered."""
        return self.action("poll_send")

    def poll_send_many(self, items: list[PollSendArgs]) -> list[dict[str, Any]]:
        """Runs `poll_send` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("poll_send", items)

    def open_sent(self, *, s: Sent) -> None:
        """Opens a request that was sent before, with what it held when it went out."""
        return self.action("open_sent", s=s)

    def open_sent_many(self, items: list[OpenSentArgs]) -> list[dict[str, Any]]:
        """Runs `open_sent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("open_sent", items)

    def save_sent(self,
        *,
        s: Sent,
        c: SavedCollection,
    ) -> Request:
        """Saves a sent request as a request in a collection, named by its route unless it had a name."""
        return self.action("save_sent", s=s, c=c)

    def delete_sent(self, *, s: Sent) -> None:
        """Forgets a sent request."""
        return self.action("delete_sent", s=s)

    def delete_sent_many(self, items: list[DeleteSentArgs]) -> list[dict[str, Any]]:
        """Runs `delete_sent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_sent", items)

    def copy_body(self) -> None:
        """Copies the response body, exactly as the response pane shows it."""
        return self.action("copy_body")

    def copy_headers(self) -> None:
        """Copies every response header, one `Name: value` per line."""
        return self.action("copy_headers")

    def copy_request(self) -> None:
        """Copies the request as authored, with its `{{variables}}` and never a credential."""
        return self.action("copy_request")

    def dismiss_notice(self) -> None:
        """Dismisses the line under the response."""
        return self.action("dismiss_notice")

    def dismiss_notice_many(self, items: list[DismissNoticeArgs]) -> list[dict[str, Any]]:
        """Runs `dismiss_notice` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("dismiss_notice", items)

    def keep_answer(self, *, answer: str) -> None:
        """Keeps the answer in the workspace, so the window and an agent read the same one."""
        return self.action("keep_answer", answer=answer)

    def keep_answer_many(self, items: list[KeepAnswerArgs]) -> list[dict[str, Any]]:
        """Runs `keep_answer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("keep_answer", items)

    def record_sent(self, *, answer: str) -> None:
        """Adds the request that just came back to the workspace's history, newest first."""
        return self.action("record_sent", answer=answer)

    def record_sent_many(self, items: list[RecordSentArgs]) -> list[dict[str, Any]]:
        """Runs `record_sent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("record_sent", items)

    def set_body_mode(self, *, mode: BodyMode) -> None:
        """Runs the app's action."""
        return self.action("set_body_mode", mode=mode)

    def set_body_mode_many(self, items: list[SetBodyModeArgs]) -> list[dict[str, Any]]:
        """Runs `set_body_mode` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_body_mode", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_request", "new_collection", "delete_collection", "delete_collection_many", "save_request", "open_request", "open_request_many", "rename_draft", "rename_draft_many", "delete_request", "delete_request_many", "delete_opened_request", "delete_opened_request_many", "new_environment", "delete_environment", "delete_environment_many", "delete_active_environment", "delete_active_environment_many", "use_environment", "use_environment_many", "no_environment", "no_environment_many", "keep_credential", "keep_credential_many", "set_scheme", "set_scheme_many", "forget_credential", "forget_credential_many", "send", "start_send", "start_send_many", "send_failed", "send_failed_many", "settle_send", "cancel_send", "cancel_send_many", "poll_send", "poll_send_many", "open_sent", "open_sent_many", "save_sent", "delete_sent", "delete_sent_many", "copy_body", "copy_headers", "copy_request", "dismiss_notice", "dismiss_notice_many", "keep_answer", "keep_answer_many", "record_sent", "record_sent_many", "set_body_mode", "set_body_mode_many"})


class SendAsk(Resource):
    """SendAsk — `tl.so.tilo.api-tester.SendAsk`."""
    __surface__ = "so.tilo.api-tester@49fd293a1113805a"
    natoiy5b: str  # method
    xg26wm2t: str  # url
    @property
    def _52ykdq3w(self) -> list[list[str]]:
        """params (`52ykdq3w` on the wire)."""
        return self.fields["52ykdq3w"]

    @_52ykdq3w.setter
    def _52ykdq3w(self, value: list[list[str]]) -> None:
        self.edit("52ykdq3w", value=value)
    @property
    def _2g54j6fl(self) -> list[list[str]]:
        """headers (`2g54j6fl` on the wire)."""
        return self.fields["2g54j6fl"]

    @_2g54j6fl.setter
    def _2g54j6fl(self, value: list[list[str]]) -> None:
        self.edit("2g54j6fl", value=value)
    @property
    def _5hvbl66x(self) -> list[list[str]]:
        """vars (`5hvbl66x` on the wire)."""
        return self.fields["5hvbl66x"]

    @_5hvbl66x.setter
    def _5hvbl66x(self, value: list[list[str]]) -> None:
        self.edit("5hvbl66x", value=value)
    o4mypkv3: str  # auth_scheme
    wgtleqed: str  # auth_username
    @property
    def _5wzmu546(self) -> str:
        """auth_header (`5wzmu546` on the wire)."""
        return self.fields["5wzmu546"]

    @_5wzmu546.setter
    def _5wzmu546(self, value: str) -> None:
        self.edit("5wzmu546", value=value)
    @property
    def _6wq4j3vq(self) -> str:
        """auth_reference (`6wq4j3vq` on the wire)."""
        return self.fields["6wq4j3vq"]

    @_6wq4j3vq.setter
    def _6wq4j3vq(self, value: str) -> None:
        self.edit("6wq4j3vq", value=value)
    dnf4fjhy: str  # body_mode
    um3yzxeu: str  # body
    m7c3owzv: list[list[str]]  # form


class KeepAsk(Resource):
    """KeepAsk — `tl.so.tilo.api-tester.KeepAsk`."""
    __surface__ = "so.tilo.api-tester@49fd293a1113805a"
    qxcspakn: str  # value


class ForgetAsk(Resource):
    """ForgetAsk — `tl.so.tilo.api-tester.ForgetAsk`."""
    __surface__ = "so.tilo.api-tester@49fd293a1113805a"
    @property
    def _42ltyzyi(self) -> str:
        """reference (`42ltyzyi` on the wire)."""
        return self.fields["42ltyzyi"]

    @_42ltyzyi.setter
    def _42ltyzyi(self, value: str) -> None:
        self.edit("42ltyzyi", value=value)


class KeptCredential(Resource):
    """KeptCredential — `tl.so.tilo.api-tester.KeptCredential`."""
    __surface__ = "so.tilo.api-tester@49fd293a1113805a"
    haaxlsd4: str  # reference


class SendState(Resource):
    """SendState — `tl.so.tilo.api-tester.SendState`."""
    __surface__ = "so.tilo.api-tester@49fd293a1113805a"
    tgafs5zj: str  # state
    vvonqua3: str  # answer
    or6k3lr7: str  # error


class Answered(Resource):
    """Answered — `tl.so.tilo.api-tester.Answered`."""
    __surface__ = "so.tilo.api-tester@49fd293a1113805a"
    s4gqhsru: int  # status
    kqbtxrg4: str  # status_text
    @property
    def _3ffoqs7c(self) -> int:
        """duration_ms (`3ffoqs7c` on the wire)."""
        return self.fields["3ffoqs7c"]

    @_3ffoqs7c.setter
    def _3ffoqs7c(self, value: int) -> None:
        self.edit("3ffoqs7c", value=value)
    aciatrpq: int  # size_bytes
    yjlhfc23: bool  # size_exact
    n36ppxll: int  # shown_bytes
    f4fheroi: bool  # binary
    loetbxur: str  # content_type
    @property
    def _5c64ziqe(self) -> str:
        """body (`5c64ziqe` on the wire)."""
        return self.fields["5c64ziqe"]

    @_5c64ziqe.setter
    def _5c64ziqe(self, value: str) -> None:
        self.edit("5c64ziqe", value=value)
    agnxdiuh: list[list[str]]  # headers


class Replay(Resource):
    """Replay — `tl.so.tilo.api-tester.Replay`."""
    __surface__ = "so.tilo.api-tester@49fd293a1113805a"
    @property
    def _7fy3fg22(self) -> str:
        """name (`7fy3fg22` on the wire)."""
        return self.fields["7fy3fg22"]

    @_7fy3fg22.setter
    def _7fy3fg22(self, value: str) -> None:
        self.edit("7fy3fg22", value=value)
    woxdrlbr: str  # method
    uehndmlh: str  # url
    dw7wgcqy: list[list[str]]  # params
    oxqptjsf: list[list[str]]  # headers
    i66no2tx: list[list[str]]  # form
    c7sufjee: str  # scheme
    c4lbngho: str  # username
    p3sqgmt6: str  # header
    wczlgfhd: str  # reference
    x5jj7sdv: str  # body_mode
    @property
    def _5ez7t24e(self) -> str:
        """body (`5ez7t24e` on the wire)."""
        return self.fields["5ez7t24e"]

    @_5ez7t24e.setter
    def _5ez7t24e(self, value: str) -> None:
        self.edit("5ez7t24e", value=value)


class ApiTester(Resource):
    """API Tester — `tl.so.tilo.api-tester`."""
    __surface__ = "so.tilo.api-tester@49fd293a1113805a"
    def forget_value(self, *, reference: str) -> None:
        """Takes a stored value off this device."""
        return self.action("forget_value", reference=reference)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"forget_value"})


