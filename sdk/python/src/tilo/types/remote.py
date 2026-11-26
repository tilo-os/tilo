"""Types for Remote Desktop (`so.tilo.remote`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `remote/<resource>`, read with
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
    "AuthMethod",
    "ConnectionKind",
    "Notice",
    "Connection",
    "Remote",
    "Session",
    "Seen",
    "TrustedKey",
    "Here",
    "KeyId",
    "Said",
    "SshAsk",
    "VncAsk",
    "KeepAsk",
    "Started",
    "SendAsk",
    "TransferAsk",
    "TiloRemote",
    "ChangeConnectionArgs",
    "PinArgs",
    "MarkUsedArgs",
    "DisconnectArgs",
    "TrustHostKeyArgs",
    "ForgetHostKeyArgs",
    "DeleteConnectionArgs",
    "CloseSessionArgs",
    "ObserveArgs",
]

AuthMethod = Literal["password", "public_key", "device_identity"]
"""How a connection signs in. The secret itself is never here: a password or a key's passphrase is kept in the workspace's vault."""

ConnectionKind = Literal["ssh", "vnc", "rdp", "tilo"]
"""What a saved connection speaks."""

ChangeConnectionArgs = TypedDict("ChangeConnectionArgs", {
    "name": "str",
    "host": "str",
    "kind": "ConnectionKind",
    "port": "int",
    "user": "str",
    "auth": "AuthMethod",
    "group": "str",
})
"""The arguments of `change_connection`."""

PinArgs = TypedDict("PinArgs", {
    "pinned": NotRequired["bool | None"],
})
"""The arguments of `pin`."""

MarkUsedArgs = TypedDict("MarkUsedArgs", {
})
"""The arguments of `mark_used`."""

DisconnectArgs = TypedDict("DisconnectArgs", {
})
"""The arguments of `disconnect`."""

TrustHostKeyArgs = TypedDict("TrustHostKeyArgs", {
    "fingerprint": "str",
})
"""The arguments of `trust_host_key`."""

ForgetHostKeyArgs = TypedDict("ForgetHostKeyArgs", {
})
"""The arguments of `forget_host_key`."""

DeleteConnectionArgs = TypedDict("DeleteConnectionArgs", {
    "c": "Connection",
})
"""The arguments of `delete_connection`."""

CloseSessionArgs = TypedDict("CloseSessionArgs", {
})
"""The arguments of `close_session`."""

ObserveArgs = TypedDict("ObserveArgs", {
})
"""The arguments of `observe`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.remote@a6480bd78ec9e836"
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


class Connection(Resource):
    """Connection — `tl.so.tilo.remote.Connection`."""
    __surface__ = "so.tilo.remote@a6480bd78ec9e836"
    ssofhrgw: str  # name
    dyz6c4fy: str  # host
    @property
    def _3zbula3z(self) -> int:
        """port (`3zbula3z` on the wire)."""
        return self.fields["3zbula3z"]

    @_3zbula3z.setter
    def _3zbula3z(self, value: int) -> None:
        self.edit("3zbula3z", value=value)
    qudrxklp: str  # user
    auxqrphf: ConnectionKind  # kind
    e54cvtei: AuthMethod  # auth
    jf7npid4: str | None  # last_used
    poxhapg7: str  # group
    qjgd4xi7: bool  # favorite

    def change_connection(self,
        *,
        name: str,
        host: str,
        kind: ConnectionKind,
        port: int,
        user: str,
        auth: AuthMethod,
        group: str,
    ) -> None:
        """Changes what a saved connection is: only what differs is written, so two people editing one connection keep each other's changes."""
        return self.action("change_connection", name=name, host=host, kind=kind, port=port, user=user, auth=auth, group=group)

    def change_connection_many(self, items: list[ChangeConnectionArgs]) -> list[dict[str, Any]]:
        """Runs `change_connection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("change_connection", items)

    def pin(self,
        *,
        pinned: bool | None = None,
    ) -> None:
        """Pins a connection to the top of the list, or unpins it."""
        args: dict[str, Any] = {}
        if pinned is not None:
            args["pinned"] = pinned
        return self.action("pin", **args)

    def pin_many(self, items: list[PinArgs]) -> list[dict[str, Any]]:
        """Runs `pin` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("pin", items)

    def keep_secret(self, *, secret: str) -> None:
        """Keeps the password or the key's passphrase `c` signs in with in the workspace's vault, as this app's own item. Search never holds it, and no shared record carries it."""
        return self.action("keep_secret", secret=secret)

    def mark_used(self) -> None:
        """Notes that a session to `c` was opened now."""
        return self.action("mark_used")

    def mark_used_many(self, items: list[MarkUsedArgs]) -> list[dict[str, Any]]:
        """Runs `mark_used` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("mark_used", items)

    def connect(self) -> Session:
        """Opens a live session to a saved connection: a terminal for SSH, the machine's screen for VNC. Nothing is sent before the saved address and sign-in are checked again."""
        return self.action("connect")

    def disconnect(self) -> None:
        """Ends the session to `c` this device has open."""
        return self.action("disconnect")

    def disconnect_many(self, items: list[DisconnectArgs]) -> list[dict[str, Any]]:
        """Runs `disconnect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("disconnect", items)

    def send_text(self, *, text: str) -> None:
        """Types `text` into an open SSH session, as if it were typed at its terminal."""
        return self.action("send_text", text=text)

    def put_file(self,
        *,
        local_path: str,
        remote_path: str,
    ) -> None:
        """Sends one file from this device to the machine an SSH session is open to."""
        return self.action("put_file", local_path=local_path, remote_path=remote_path)

    def fetch_file(self,
        *,
        remote_path: str,
        local_path: str,
    ) -> None:
        """Fetches one file from the machine an SSH session is open to, onto this device."""
        return self.action("fetch_file", remote_path=remote_path, local_path=local_path)

    def moved_file(self, *, j: Any) -> None:
        """Says how a file's way to or from a machine ended."""
        return self.action("moved_file", j=j)

    def trust_host_key(self, *, fingerprint: str) -> None:
        """Trusts the SSH key a machine showed on first contact, on this device only; connecting again signs in. The key is checked against the one shown: a machine that changed it since is asked about again."""
        return self.action("trust_host_key", fingerprint=fingerprint)

    def trust_host_key_many(self, items: list[TrustHostKeyArgs]) -> list[dict[str, Any]]:
        """Runs `trust_host_key` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("trust_host_key", items)

    def forget_host_key(self) -> None:
        """Forgets the key this device trusted for a machine whose key changed; connecting again shows the new one to check."""
        return self.action("forget_host_key")

    def forget_host_key_many(self, items: list[ForgetHostKeyArgs]) -> list[dict[str, Any]]:
        """Runs `forget_host_key` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("forget_host_key", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"change_connection", "change_connection_many", "pin", "pin_many", "keep_secret", "mark_used", "mark_used_many", "connect", "disconnect", "disconnect_many", "send_text", "put_file", "fetch_file", "moved_file", "trust_host_key", "trust_host_key_many", "forget_host_key", "forget_host_key_many"})


class Remote(Resource):
    """Remote — `tl.so.tilo.remote.Remote`."""
    __surface__ = "so.tilo.remote@a6480bd78ec9e836"
    yylg63a7: list[Entry[Connection]]  # connections

    def add_connection(self,
        *,
        name: str,
        host: str,
        kind: ConnectionKind | None = None,
        port: int | None = None,
        user: str | None = None,
        auth: AuthMethod | None = None,
        group: str | None = None,
    ) -> Connection:
        """Saves a machine to reach over SSH, VNC or as a Tilo device."""
        args: dict[str, Any] = {}
        args["name"] = name
        args["host"] = host
        if kind is not None:
            args["kind"] = kind
        if port is not None:
            args["port"] = port
        if user is not None:
            args["user"] = user
        if auth is not None:
            args["auth"] = auth
        if group is not None:
            args["group"] = group
        return self.action("add_connection", **args)

    def delete_connection(self, *, c: Connection) -> None:
        """Deletes a saved connection for everyone it is shared with; the machine itself is not changed. A password kept for it stays in the workspace's vault until it is forgotten in Passwords."""
        return self.action("delete_connection", c=c)

    def delete_connection_many(self, items: list[DeleteConnectionArgs]) -> list[dict[str, Any]]:
        """Runs `delete_connection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete_connection", items)

    def save_form(self,
        *,
        name: str,
        host: str,
        port: str,
        user: str,
        kind: ConnectionKind,
        auth: AuthMethod,
        group: str,
        secret: str,
        editing: Connection | None = None,
    ) -> bool:
        """Saves the form: adds the connection or changes the one it was opened over, and keeps the password in the workspace's vault. Answers whether it saved; a refusal leaves the form as it was."""
        args: dict[str, Any] = {}
        if editing is not None:
            args["editing"] = editing
        args["name"] = name
        args["host"] = host
        args["port"] = port
        args["user"] = user
        args["kind"] = kind
        args["auth"] = auth
        args["group"] = group
        args["secret"] = secret
        return self.action("save_form", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"add_connection", "delete_connection", "delete_connection_many", "save_form"})


class Session(Resource):
    """Session — `tl.so.tilo.remote.Session`."""
    __surface__ = "so.tilo.remote@a6480bd78ec9e836"
    fpphznkb: str  # connection
    vvyt5lha: str  # name
    amv62hzp: ConnectionKind  # kind
    l3jskskl: str  # job
    @property
    def _3cerrtf7(self) -> str:
        """terminal (`3cerrtf7` on the wire)."""
        return self.fields["3cerrtf7"]

    @_3cerrtf7.setter
    def _3cerrtf7(self, value: str) -> None:
        self.edit("3cerrtf7", value=value)
    gazx3juc: str  # trouble

    def close_session(self) -> None:
        """Ends a session; the saved connection stays."""
        return self.action("close_session")

    def close_session_many(self, items: list[CloseSessionArgs]) -> list[dict[str, Any]]:
        """Runs `close_session` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("close_session", items)

    def observe(self) -> None:
        """Notes what this device saw of the machine a session reached: up once it answered, down once it failed to."""
        return self.action("observe")

    def observe_many(self, items: list[ObserveArgs]) -> list[dict[str, Any]]:
        """Runs `observe` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("observe", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"close_session", "close_session_many", "observe", "observe_many"})


class Seen(Resource):
    """Seen — `tl.so.tilo.remote.Seen`."""
    __surface__ = "so.tilo.remote@a6480bd78ec9e836"
    rb2plwyy: str  # connection
    hgy5dnvw: bool  # online


class TrustedKey(Resource):
    """TrustedKey — `tl.so.tilo.remote.TrustedKey`."""
    __surface__ = "so.tilo.remote@a6480bd78ec9e836"
    rgmdf7xq: str  # host
    cxui2vf5: int  # port
    bmf56c7k: str  # algorithm
    n3ikzj6y: str  # fingerprint


class Here(Resource):
    """Here — `tl.so.tilo.remote.Here`."""
    __surface__ = "so.tilo.remote@a6480bd78ec9e836"
    mqn5woua: list[Entry[Session]]  # sessions
    b32nsjwp: list[Entry[Seen]]  # seen
    jbjh2xut: list[Entry[TrustedKey]]  # trusted


class KeyId(Resource):
    """KeyId — `tl.so.tilo.remote.KeyId`."""
    __surface__ = "so.tilo.remote@a6480bd78ec9e836"
    x5gkgjv7: str  # algorithm
    @property
    def _5vpzlqkt(self) -> str:
        """fingerprint (`5vpzlqkt` on the wire)."""
        return self.fields["5vpzlqkt"]

    @_5vpzlqkt.setter
    def _5vpzlqkt(self, value: str) -> None:
        self.edit("5vpzlqkt", value=value)


class Said(Resource):
    """Said — `tl.so.tilo.remote.Said`."""
    __surface__ = "so.tilo.remote@a6480bd78ec9e836"
    azv5q3lb: str  # phase
    app3bmk2: str  # why
    @property
    def _5owfkzza(self) -> str:
        """algorithm (`5owfkzza` on the wire)."""
        return self.fields["5owfkzza"]

    @_5owfkzza.setter
    def _5owfkzza(self, value: str) -> None:
        self.edit("5owfkzza", value=value)
    unhnci5d: str  # fingerprint
    gwkgueek: KeyId | None  # expected
    pqs4atu7: KeyId | None  # observed


class SshAsk(Resource):
    """SshAsk — `tl.so.tilo.remote.SshAsk`."""
    __surface__ = "so.tilo.remote@a6480bd78ec9e836"
    qyt6kg5x: str  # host
    hey2mljd: int  # port
    lutwl6ee: str  # user
    iiovxv6t: str  # method
    x4aolsn5: str  # secret_key
    swuhzzts: KeyId | None  # expected
    i4deiyeq: int  # columns
    wrbw55me: int  # rows


class VncAsk(Resource):
    """VncAsk — `tl.so.tilo.remote.VncAsk`."""
    __surface__ = "so.tilo.remote@a6480bd78ec9e836"
    jtksl5yz: str  # host
    ypo5oexy: int  # port
    @property
    def _7re6rxst(self) -> str:
        """password_key (`7re6rxst` on the wire)."""
        return self.fields["7re6rxst"]

    @_7re6rxst.setter
    def _7re6rxst(self, value: str) -> None:
        self.edit("7re6rxst", value=value)


class KeepAsk(Resource):
    """KeepAsk — `tl.so.tilo.remote.KeepAsk`."""
    __surface__ = "so.tilo.remote@a6480bd78ec9e836"
    z6sarmt3: str  # key
    @property
    def _4zetse6n(self) -> str:
        """secret (`4zetse6n` on the wire)."""
        return self.fields["4zetse6n"]

    @_4zetse6n.setter
    def _4zetse6n(self, value: str) -> None:
        self.edit("4zetse6n", value=value)


class Started(Resource):
    """Started — `tl.so.tilo.remote.Started`."""
    __surface__ = "so.tilo.remote@a6480bd78ec9e836"
    hk6v4nxp: str  # job
    k6qck3ko: str  # session


class SendAsk(Resource):
    """SendAsk — `tl.so.tilo.remote.SendAsk`."""
    __surface__ = "so.tilo.remote@a6480bd78ec9e836"
    jlt5sla7: str  # job
    @property
    def _5kuradb3(self) -> str:
        """text (`5kuradb3` on the wire)."""
        return self.fields["5kuradb3"]

    @_5kuradb3.setter
    def _5kuradb3(self, value: str) -> None:
        self.edit("5kuradb3", value=value)


class TransferAsk(Resource):
    """TransferAsk — `tl.so.tilo.remote.TransferAsk`."""
    __surface__ = "so.tilo.remote@a6480bd78ec9e836"
    @property
    def _376cxznk(self) -> str:
        """job (`376cxznk` on the wire)."""
        return self.fields["376cxznk"]

    @_376cxznk.setter
    def _376cxznk(self, value: str) -> None:
        self.edit("376cxznk", value=value)
    da2gioaf: str  # local_path
    @property
    def _33n5eruw(self) -> str:
        """remote_path (`33n5eruw` on the wire)."""
        return self.fields["33n5eruw"]

    @_33n5eruw.setter
    def _33n5eruw(self, value: str) -> None:
        self.edit("33n5eruw", value=value)


class TiloRemote(Resource):
    """Remote Desktop — `tl.so.tilo.remote`."""
    __surface__ = "so.tilo.remote@a6480bd78ec9e836"

