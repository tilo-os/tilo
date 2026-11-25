"""Types for Authenticator (`so.tilo.totp`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `totp/<resource>`, read with
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
    "Notice",
    "Account",
    "Vault",
    "ParseAsk",
    "ParsedAnswer",
    "ScanAsk",
    "ScanAnswer",
    "CheckAsk",
    "CheckAnswer",
    "Totp",
    "RecolorAccountArgs",
    "MoveUpArgs",
    "MoveDownArgs",
    "ClearNoticeArgs",
]

RecolorAccountArgs = TypedDict("RecolorAccountArgs", {
    "color": "int",
})
"""The arguments of `recolor_account`."""

MoveUpArgs = TypedDict("MoveUpArgs", {
    "a": "Account",
})
"""The arguments of `move_up`."""

MoveDownArgs = TypedDict("MoveDownArgs", {
    "a": "Account",
})
"""The arguments of `move_down`."""

ClearNoticeArgs = TypedDict("ClearNoticeArgs", {
})
"""The arguments of `clear_notice`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.totp@3e62ea2429cdfe14"
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


class Account(Resource):
    """Account — `tl.so.tilo.totp.Account`."""
    __surface__ = "so.tilo.totp@3e62ea2429cdfe14"
    wn7r7rgq: str  # issuer
    k6loujfj: str  # label
    sj2mtsnu: str  # item
    @property
    def _2f4aknif(self) -> int:
        """color (`2f4aknif` on the wire)."""
        return self.fields["2f4aknif"]

    @_2f4aknif.setter
    def _2f4aknif(self, value: int) -> None:
        self.edit("2f4aknif", value=value)
    g3aq6san: str  # tuned_at

    def rename_account(self,
        *,
        issuer: str,
        algorithm: str,
        digits: int,
        period: int,
    ) -> None:
        """Renames an account's service: what a person changing the name it shows did. The codes' settings come from the form, which read them when it opened: an action never reads the vault, so what it keeps is what the window showed the person."""
        return self.action("rename_account", issuer=issuer, algorithm=algorithm, digits=digits, period=period)

    def relabel_account(self,
        *,
        label: str,
        algorithm: str,
        digits: int,
        period: int,
    ) -> None:
        """Changes who an account signs in as: what a person changing its name did. Its codes' settings come from the form, as a rename's do."""
        return self.action("relabel_account", label=label, algorithm=algorithm, digits=digits, period=period)

    def recolor_account(self, *, color: int) -> None:
        """Gives an account a colour of its own: what a person recolouring it did."""
        return self.action("recolor_account", color=color)

    def recolor_account_many(self, items: list[RecolorAccountArgs]) -> list[dict[str, Any]]:
        """Runs `recolor_account` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("recolor_account", items)

    def recode_account(self,
        *,
        algorithm: str,
        digits: int,
        period: int,
        secret: str | None = None,
    ) -> None:
        """Changes how an account makes its codes, and its key with them: what a person changing the algorithm, the digits, the period or the key did."""
        args: dict[str, Any] = {}
        args["algorithm"] = algorithm
        args["digits"] = digits
        args["period"] = period
        if secret is not None:
            args["secret"] = secret
        return self.action("recode_account", **args)

    def update_account(self,
        *,
        issuer: str,
        label: str | None = None,
        secret: str | None = None,
        algorithm: str | None = None,
        digits: int | None = None,
        period: int | None = None,
    ) -> None:
        """Changes what an account says, and its key and how its codes are made. A key left out keeps the one the vault holds: the service's key never leaves it, so changing an account's details can never show it."""
        args: dict[str, Any] = {}
        args["issuer"] = issuer
        if label is not None:
            args["label"] = label
        if secret is not None:
            args["secret"] = secret
        if algorithm is not None:
            args["algorithm"] = algorithm
        if digits is not None:
            args["digits"] = digits
        if period is not None:
            args["period"] = period
        return self.action("update_account", **args)

    def copy_code(self) -> None:
        """Copies an account's current code to the clipboard, and says so: the person asked for it, and a window that shows nothing would leave them guessing whether it worked."""
        return self.action("copy_code")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"rename_account", "relabel_account", "recolor_account", "recolor_account_many", "recode_account", "update_account", "copy_code"})


class Vault(Resource):
    """Vault — `tl.so.tilo.totp.Vault`."""
    __surface__ = "so.tilo.totp@3e62ea2429cdfe14"
    @property
    def _4rdnjfde(self) -> list[Entry[Account]]:
        """accounts (`4rdnjfde` on the wire)."""
        return self.fields["4rdnjfde"]

    @_4rdnjfde.setter
    def _4rdnjfde(self, value: list[Entry[Account]]) -> None:
        self.edit("4rdnjfde", value=value)
    tcfimqtj: str  # notice
    glvalakf: str  # notice_kind
    djbrztzn: str  # pending
    abeenkwo: str  # pending_issuer
    t2vgrcpe: str  # pending_label
    kqaty43e: str  # pending_algorithm
    nlfjadyy: int  # pending_digits
    ouj3ko22: int  # pending_period

    def add_account(self,
        *,
        issuer: str,
        secret: str,
        label: str | None = None,
        algorithm: str | None = None,
        digits: int | None = None,
        period: int | None = None,
        color: int | None = None,
    ) -> Account:
        """Starts an account from what the person typed, or what a setup link said: what an agent, a script or the form asks. The vault keeps the seed; the account keeps the words."""
        args: dict[str, Any] = {}
        args["issuer"] = issuer
        if label is not None:
            args["label"] = label
        args["secret"] = secret
        if algorithm is not None:
            args["algorithm"] = algorithm
        if digits is not None:
            args["digits"] = digits
        if period is not None:
            args["period"] = period
        if color is not None:
            args["color"] = color
        return self.action("add_account", **args)

    def import_uri(self, *, uri: str) -> Account:
        """Adds the account a setup link names: what the QR scan and the paste field both end in."""
        return self.action("import_uri", uri=uri)

    def scan_picture(self, *, file: Any) -> None:
        """Reads the account a picture's QR code names, and keeps its key in the vault for the form to show: nothing is added until the person reads what the picture said and saves it."""
        return self.action("scan_picture", file=file)

    def add_pending(self) -> Account:
        """Adds the account the picture read, with the key the vault already holds for it."""
        return self.action("add_pending")

    def drop_pending(self) -> None:
        """Lets the picture's account go: the key the vault kept for it is forgotten too."""
        return self.action("drop_pending")

    def move_up(self, *, a: Account) -> None:
        """Moves an account one place up the list."""
        return self.action("move_up", a=a)

    def move_up_many(self, items: list[MoveUpArgs]) -> list[dict[str, Any]]:
        """Runs `move_up` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_up", items)

    def move_down(self, *, a: Account) -> None:
        """Moves an account one place down the list."""
        return self.action("move_down", a=a)

    def move_down_many(self, items: list[MoveDownArgs]) -> list[dict[str, Any]]:
        """Runs `move_down` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("move_down", items)

    def delete_account(self, *, a: Account) -> None:
        """Deletes an account and the key the vault kept for it."""
        return self.action("delete_account", a=a)

    def refuse(self, *, why: str) -> Account:
        """Runs the app's action."""
        return self.action("refuse", why=why)

    def save_account(self,
        *,
        issuer: str,
        label: str,
        secret: str,
        algorithm: str,
        digits: int,
        period: int,
        editing: Account | None = None,
        color: int | None = None,
        was_algorithm: str | None = None,
        was_digits: int | None = None,
        was_period: int | None = None,
    ) -> bool:
        """Saves the form: starts the account, or changes the one it was opened over, answering whether it saved. A refusal says why in the bar and leaves the form open with what was typed, so a window only leaves the form when this answered that it saved."""
        args: dict[str, Any] = {}
        if editing is not None:
            args["editing"] = editing
        args["issuer"] = issuer
        args["label"] = label
        args["secret"] = secret
        args["algorithm"] = algorithm
        args["digits"] = digits
        args["period"] = period
        if color is not None:
            args["color"] = color
        if was_algorithm is not None:
            args["was_algorithm"] = was_algorithm
        if was_digits is not None:
            args["was_digits"] = was_digits
        if was_period is not None:
            args["was_period"] = was_period
        return self.action("save_account", **args)

    def clear_notice(self) -> None:
        """Runs the app's action."""
        return self.action("clear_notice")

    def clear_notice_many(self, items: list[ClearNoticeArgs]) -> list[dict[str, Any]]:
        """Runs `clear_notice` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_notice", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"add_account", "import_uri", "scan_picture", "add_pending", "drop_pending", "move_up", "move_up_many", "move_down", "move_down_many", "delete_account", "refuse", "save_account", "clear_notice", "clear_notice_many"})


class ParseAsk(Resource):
    """ParseAsk — `tl.so.tilo.totp.ParseAsk`."""
    __surface__ = "so.tilo.totp@3e62ea2429cdfe14"
    qhgdgyyn: str  # uri


class ParsedAnswer(Resource):
    """ParsedAnswer — `tl.so.tilo.totp.ParsedAnswer`."""
    __surface__ = "so.tilo.totp@3e62ea2429cdfe14"
    x5elnfmo: str  # issuer
    ac2s7eqm: str  # label
    nsnnmxnd: str  # secret
    @property
    def _234azfht(self) -> str:
        """algorithm (`234azfht` on the wire)."""
        return self.fields["234azfht"]

    @_234azfht.setter
    def _234azfht(self, value: str) -> None:
        self.edit("234azfht", value=value)
    cikeo42j: int  # digits
    p5k5ujrm: int  # period


class ScanAsk(Resource):
    """ScanAsk — `tl.so.tilo.totp.ScanAsk`."""
    __surface__ = "so.tilo.totp@3e62ea2429cdfe14"
    j757xqu7: str  # address


class ScanAnswer(Resource):
    """ScanAnswer — `tl.so.tilo.totp.ScanAnswer`."""
    __surface__ = "so.tilo.totp@3e62ea2429cdfe14"
    @property
    def _4jvzelry(self) -> str:
        """uri (`4jvzelry` on the wire)."""
        return self.fields["4jvzelry"]

    @_4jvzelry.setter
    def _4jvzelry(self, value: str) -> None:
        self.edit("4jvzelry", value=value)


class CheckAsk(Resource):
    """CheckAsk — `tl.so.tilo.totp.CheckAsk`."""
    __surface__ = "so.tilo.totp@3e62ea2429cdfe14"
    shi3ofgj: str  # secret


class CheckAnswer(Resource):
    """CheckAnswer — `tl.so.tilo.totp.CheckAnswer`."""
    __surface__ = "so.tilo.totp@3e62ea2429cdfe14"
    pon234hn: bool  # ok


class Totp(Resource):
    """Authenticator — `tl.so.tilo.totp`."""
    __surface__ = "so.tilo.totp@3e62ea2429cdfe14"

