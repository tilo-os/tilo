"""Types for App Publisher (`so.tilo.publisher`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Reading` is the app's `tl.so.tilo.publisher.Reading` at `publisher/<resource>`, read with
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
    "Reading",
    "Publisher",
    "ForgeParts",
    "KeyFile",
    "ApplyAsk",
    "StatusAsk",
    "JobState",
    "JobAnswer",
    "KeyState",
    "TiloPublisher",
    "ReadKeyArgs",
    "CreateKeyArgs",
    "ApplyForNamespacesArgs",
    "CheckStatusArgs",
    "PollArgs",
]

ReadKeyArgs = TypedDict("ReadKeyArgs", {
})
"""The arguments of `read_key`."""

CreateKeyArgs = TypedDict("CreateKeyArgs", {
})
"""The arguments of `create_key`."""

ApplyForNamespacesArgs = TypedDict("ApplyForNamespacesArgs", {
})
"""The arguments of `apply_for_namespaces`."""

CheckStatusArgs = TypedDict("CheckStatusArgs", {
})
"""The arguments of `check_status`."""

PollArgs = TypedDict("PollArgs", {
})
"""The arguments of `poll`."""

class Reading(Resource):
    """Reading — `tl.so.tilo.publisher.Reading`."""
    __surface__ = "so.tilo.publisher@10d59cfeee8fcfea"
    hia3wxhi: str  # at
    ntbsg676: str  # summary


class Publisher(Resource):
    """Publisher — `tl.so.tilo.publisher.Publisher`."""
    __surface__ = "so.tilo.publisher@10d59cfeee8fcfea"
    izqbnlov: str  # registry
    @property
    def _4hwurbpa(self) -> str:
        """domain (`4hwurbpa` on the wire)."""
        return self.fields["4hwurbpa"]

    @_4hwurbpa.setter
    def _4hwurbpa(self, value: str) -> None:
        self.edit("4hwurbpa", value=value)
    @property
    def _6fcxou37(self) -> str:
        """forge_account (`6fcxou37` on the wire)."""
        return self.fields["6fcxou37"]

    @_6fcxou37.setter
    def _6fcxou37(self, value: str) -> None:
        self.edit("6fcxou37", value=value)
    @property
    def _22elkpx4(self) -> str:
        """contact (`22elkpx4` on the wire)."""
        return self.fields["22elkpx4"]

    @_22elkpx4.setter
    def _22elkpx4(self, value: str) -> None:
        self.edit("22elkpx4", value=value)
    rgwkewz7: str  # key
    b63dald4: str  # public_key
    b6h3el5f: str  # notice_kind
    laxvdhhk: str  # notice_title
    ara6pea6: str  # notice_detail
    vu53jaba: str  # job
    etzmfdag: list[Entry[Reading]]  # readings

    def read_key(self) -> None:
        """Reads this computer's publisher key: whether it has one, and its public half. Opening the app reads it, so the window shows the key's state without anything to press."""
        return self.action("read_key")

    def read_key_many(self, items: list[ReadKeyArgs]) -> list[dict[str, Any]]:
        """Runs `read_key` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("read_key", items)

    def create_key(self) -> None:
        """Makes this computer's publisher key. It signs the person's apps and their requests to the registry, and a second one is never made: a publisher key is one per machine."""
        return self.action("create_key")

    def create_key_many(self, items: list[CreateKeyArgs]) -> list[dict[str, Any]]:
        """Runs `create_key` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("create_key", items)

    def copy_proof(self) -> None:
        """Copies the proof line: the words to publish where only the person can write."""
        return self.action("copy_proof")

    def copy_secret(self) -> None:
        """Copies the key file, for the person's CI: one secret, `TILO_PUBLISHER_KEY`, holding it."""
        return self.action("copy_secret")

    def export_key(self) -> Any:
        """The key file, for the person to keep somewhere safe: what they restore the key from."""
        return self.action("export_key")

    def apply_for_namespaces(self) -> None:
        """Asks the registry for the namespaces the profile's domain and forge account name."""
        return self.action("apply_for_namespaces")

    def apply_for_namespaces_many(self, items: list[ApplyForNamespacesArgs]) -> list[dict[str, Any]]:
        """Runs `apply_for_namespaces` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("apply_for_namespaces", items)

    def check_status(self) -> None:
        """Reads what the registry holds about this computer's key's application."""
        return self.action("check_status")

    def check_status_many(self, items: list[CheckStatusArgs]) -> list[dict[str, Any]]:
        """Runs `check_status` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("check_status", items)

    def poll(self) -> None:
        """Asks the registry how the call under way is doing, once a second while one runs."""
        return self.action("poll")

    def poll_many(self, items: list[PollArgs]) -> list[dict[str, Any]]:
        """Runs `poll` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("poll", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"read_key", "read_key_many", "create_key", "create_key_many", "copy_proof", "copy_secret", "export_key", "apply_for_namespaces", "apply_for_namespaces_many", "check_status", "check_status_many", "poll", "poll_many"})


class ForgeParts(Resource):
    """ForgeParts — `tl.so.tilo.publisher.ForgeParts`."""
    __surface__ = "so.tilo.publisher@10d59cfeee8fcfea"
    y36hrf57: str  # host
    iwdc7ejj: str  # account


class KeyFile(Resource):
    """KeyFile — `tl.so.tilo.publisher.KeyFile`."""
    __surface__ = "so.tilo.publisher@10d59cfeee8fcfea"
    @property
    def _6347lg4i(self) -> str:
        """text (`6347lg4i` on the wire)."""
        return self.fields["6347lg4i"]

    @_6347lg4i.setter
    def _6347lg4i(self, value: str) -> None:
        self.edit("6347lg4i", value=value)


class ApplyAsk(Resource):
    """ApplyAsk — `tl.so.tilo.publisher.ApplyAsk`."""
    __surface__ = "so.tilo.publisher@10d59cfeee8fcfea"
    gh566qcn: str  # registry
    eb4v6asr: str  # domain
    hjj2vyhq: str  # forge_account
    e2wp4tw7: str  # contact


class StatusAsk(Resource):
    """StatusAsk — `tl.so.tilo.publisher.StatusAsk`."""
    __surface__ = "so.tilo.publisher@10d59cfeee8fcfea"
    lbble236: str  # registry


class JobState(Resource):
    """JobState — `tl.so.tilo.publisher.JobState`."""
    __surface__ = "so.tilo.publisher@10d59cfeee8fcfea"
    jqgcvhvb: str  # state
    wu2vhifg: str  # summary
    ghygxnrv: str  # error


class JobAnswer(Resource):
    """JobAnswer — `tl.so.tilo.publisher.JobAnswer`."""
    __surface__ = "so.tilo.publisher@10d59cfeee8fcfea"
    q55hkx2s: str  # job


class KeyState(Resource):
    """KeyState — `tl.so.tilo.publisher.KeyState`."""
    __surface__ = "so.tilo.publisher@10d59cfeee8fcfea"
    iyvbvvra: str  # state
    @property
    def _5gktvyxf(self) -> str:
        """public (`5gktvyxf` on the wire)."""
        return self.fields["5gktvyxf"]

    @_5gktvyxf.setter
    def _5gktvyxf(self, value: str) -> None:
        self.edit("5gktvyxf", value=value)


class TiloPublisher(Resource):
    """App Publisher — `tl.so.tilo.publisher`."""
    __surface__ = "so.tilo.publisher@10d59cfeee8fcfea"

