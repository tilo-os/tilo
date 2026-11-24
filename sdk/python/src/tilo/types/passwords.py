"""Types for Passwords (`so.tilo.passwords`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Item` is the app's `tl.so.tilo.passwords.Item` at `passwords/<resource>`, read with
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
    "Kind",
    "Item",
    "ImportRow",
    "Vault",
    "LoginList",
    "LoginEntry",
    "TypedAsk",
    "AskedFile",
    "ImportAsk",
    "CsvRow",
    "RowsAnswer",
    "CountAnswer",
    "RestoredAnswer",
    "CarriedAnswer",
    "UrlAnswer",
    "Passwords",
    "KeepFactsArgs",
    "SetUsernameArgs",
    "GeneratePasswordArgs",
    "FavoriteArgs",
    "AddTagArgs",
    "RemoveTagArgs",
    "AddOriginArgs",
    "RemoveOriginArgs",
    "RenameArgs",
    "SetKindArgs",
    "CarryArgs",
    "DeleteArgs",
    "SettleArgs",
    "PreviewCsvArgs",
    "ApplyImportArgs",
    "ClearImportArgs",
    "AbsorbDeviceArgs",
]

Kind = Literal["login", "api_key", "note", "wifi", "secret"]
"""What kind of secret an item is: what it is for, which sets the fields it has and the words that name them."""

KeepFactsArgs = TypedDict("KeepFactsArgs", {
    "username": "str",
    "origins": "list[str]",
})
"""The arguments of `keep_facts`."""

SetUsernameArgs = TypedDict("SetUsernameArgs", {
    "username": "str",
})
"""The arguments of `set_username`."""

GeneratePasswordArgs = TypedDict("GeneratePasswordArgs", {
    "length": NotRequired["int | None"],
    "capitals": NotRequired["bool | None"],
    "digits": NotRequired["bool | None"],
    "symbols": NotRequired["bool | None"],
})
"""The arguments of `generate_password`."""

FavoriteArgs = TypedDict("FavoriteArgs", {
    "favorite": NotRequired["bool | None"],
})
"""The arguments of `favorite`."""

AddTagArgs = TypedDict("AddTagArgs", {
    "tag": NotRequired["str | None"],
})
"""The arguments of `add_tag`."""

RemoveTagArgs = TypedDict("RemoveTagArgs", {
    "tag": "str",
})
"""The arguments of `remove_tag`."""

AddOriginArgs = TypedDict("AddOriginArgs", {
    "origin": NotRequired["str | None"],
})
"""The arguments of `add_origin`."""

RemoveOriginArgs = TypedDict("RemoveOriginArgs", {
    "origin": "str",
})
"""The arguments of `remove_origin`."""

RenameArgs = TypedDict("RenameArgs", {
    "title": "str",
})
"""The arguments of `rename`."""

SetKindArgs = TypedDict("SetKindArgs", {
    "kind": "Kind",
})
"""The arguments of `set_kind`."""

CarryArgs = TypedDict("CarryArgs", {
})
"""The arguments of `carry`."""

DeleteArgs = TypedDict("DeleteArgs", {
    "item": "Item",
})
"""The arguments of `delete`."""

SettleArgs = TypedDict("SettleArgs", {
})
"""The arguments of `settle`."""

PreviewCsvArgs = TypedDict("PreviewCsvArgs", {
    "file": "Any",
})
"""The arguments of `preview_csv`."""

ApplyImportArgs = TypedDict("ApplyImportArgs", {
})
"""The arguments of `apply_import`."""

ClearImportArgs = TypedDict("ClearImportArgs", {
})
"""The arguments of `clear_import`."""

AbsorbDeviceArgs = TypedDict("AbsorbDeviceArgs", {
})
"""The arguments of `absorb_device`."""

class Item(Resource):
    """Item — `tl.so.tilo.passwords.Item`."""
    __surface__ = "so.tilo.passwords@dac1b67d7a707940"
    vvthph26: str  # entry
    ildzcyo2: str  # title
    ujkdowq4: Kind  # kind
    qoa35hei: list[str]  # tags
    tzeujwn7: bool  # favorite
    tpqopgvl: str  # username
    txdg33mm: str  # password
    fxvfkz3x: str  # notes
    qgllbsrc: list[str]  # origins
    japk242o: dict[str, str]  # fields

    def keep_facts(self,
        *,
        username: str,
        origins: list[str],
    ) -> None:
        """Writes what is safe to say of an item into its vault entry: who it signs in as and where, with the item's name, kind and tags."""
        return self.action("keep_facts", username=username, origins=origins)

    def keep_facts_many(self, items: list[KeepFactsArgs]) -> list[dict[str, Any]]:
        """Runs `keep_facts` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("keep_facts", items)

    def set_username(self, *, username: str) -> None:
        """Writes who the item signs in as."""
        return self.action("set_username", username=username)

    def set_username_many(self, items: list[SetUsernameArgs]) -> list[dict[str, Any]]:
        """Runs `set_username` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_username", items)

    def change_secret(self) -> str:
        """Changes the item's password, key or secret: the person types it on the system's card, and the vault keeps it. Nothing of it reaches this app's code, its records or history."""
        return self.action("change_secret")

    def write_note(self) -> str:
        """Writes the item's note: typed on the system's card, kept in the vault."""
        return self.action("write_note")

    def add_field(self,
        *,
        label: str | None = None,
    ) -> str:
        """Adds a field to the item, such as a PIN: its value typed on the system's card."""
        args: dict[str, Any] = {}
        if label is not None:
            args["label"] = label
        return self.action("add_field", **args)

    def remove_field(self, *, label: str) -> str:
        """Takes a field off the item."""
        return self.action("remove_field", label=label)

    def generate_password(self,
        *,
        length: int | None = None,
        capitals: bool | None = None,
        digits: bool | None = None,
        symbols: bool | None = None,
    ) -> None:
        """Draws a strong password for the item where the vault is: 20 characters by default, with a capital, a small letter, a digit and a symbol. Nobody types or sees it."""
        args: dict[str, Any] = {}
        if length is not None:
            args["length"] = length
        if capitals is not None:
            args["capitals"] = capitals
        if digits is not None:
            args["digits"] = digits
        if symbols is not None:
            args["symbols"] = symbols
        return self.action("generate_password", **args)

    def generate_password_many(self, items: list[GeneratePasswordArgs]) -> list[dict[str, Any]]:
        """Runs `generate_password` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("generate_password", items)

    def favorite(self,
        *,
        favorite: bool | None = None,
    ) -> None:
        """Puts an item in the person's favorites, or takes it out."""
        args: dict[str, Any] = {}
        if favorite is not None:
            args["favorite"] = favorite
        return self.action("favorite", **args)

    def favorite_many(self, items: list[FavoriteArgs]) -> list[dict[str, Any]]:
        """Runs `favorite` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("favorite", items)

    def add_tag(self,
        *,
        tag: str | None = None,
    ) -> None:
        """Gives an item a tag, as the words beside its tags are typed."""
        args: dict[str, Any] = {}
        if tag is not None:
            args["tag"] = tag
        return self.action("add_tag", **args)

    def add_tag_many(self, items: list[AddTagArgs]) -> list[dict[str, Any]]:
        """Runs `add_tag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_tag", items)

    def remove_tag(self, *, tag: str) -> None:
        """Takes a tag off an item."""
        return self.action("remove_tag", tag=tag)

    def remove_tag_many(self, items: list[RemoveTagArgs]) -> list[dict[str, Any]]:
        """Runs `remove_tag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_tag", items)

    def add_origin(self,
        *,
        origin: str | None = None,
    ) -> None:
        """Gives an item an address it signs in to, checked as a web address, as typed beside them."""
        args: dict[str, Any] = {}
        if origin is not None:
            args["origin"] = origin
        return self.action("add_origin", **args)

    def add_origin_many(self, items: list[AddOriginArgs]) -> list[dict[str, Any]]:
        """Runs `add_origin` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("add_origin", items)

    def remove_origin(self, *, origin: str) -> None:
        """Takes an address off an item."""
        return self.action("remove_origin", origin=origin)

    def remove_origin_many(self, items: list[RemoveOriginArgs]) -> list[dict[str, Any]]:
        """Runs `remove_origin` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remove_origin", items)

    def rename(self, *, title: str) -> None:
        """Renames an item, in its record and its vault entry."""
        return self.action("rename", title=title)

    def rename_many(self, items: list[RenameArgs]) -> list[dict[str, Any]]:
        """Runs `rename` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename", items)

    def set_kind(self, *, kind: Kind) -> None:
        """Changes what kind of secret an item is."""
        return self.action("set_kind", kind=kind)

    def set_kind_many(self, items: list[SetKindArgs]) -> list[dict[str, Any]]:
        """Runs `set_kind` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_kind", items)

    def copy_secret(self) -> None:
        """Copies the item's secret to the person's clipboard, which forgets it again after a while. Copying reads the vault and keeps nothing."""
        return self.action("copy_secret")

    def copy_identity(self) -> None:
        """Copies what identifies the person to the account: a login's username, an API key's account, a Wi-Fi network's name."""
        return self.action("copy_identity")

    def copy_origins(self) -> None:
        """Copies the addresses the item signs in to, one a line."""
        return self.action("copy_origins")

    def carry(self) -> None:
        """Moves what an item's record keeps into the vault, once, and empties the record: an entry the vault already keeps under the item's id stays as it is."""
        return self.action("carry")

    def carry_many(self, items: list[CarryArgs]) -> list[dict[str, Any]]:
        """Runs `carry` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("carry", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"keep_facts", "keep_facts_many", "set_username", "set_username_many", "change_secret", "write_note", "add_field", "remove_field", "generate_password", "generate_password_many", "favorite", "favorite_many", "add_tag", "add_tag_many", "remove_tag", "remove_tag_many", "add_origin", "add_origin_many", "remove_origin", "remove_origin_many", "rename", "rename_many", "set_kind", "set_kind_many", "copy_secret", "copy_identity", "copy_origins", "carry", "carry_many"})


class ImportRow(Resource):
    """ImportRow — `tl.so.tilo.passwords.ImportRow`."""
    __surface__ = "so.tilo.passwords@dac1b67d7a707940"
    l7jn4eol: str  # title
    s6pvezan: str  # username
    @property
    def _4jcknnmq(self) -> str:
        """origin (`4jcknnmq` on the wire)."""
        return self.fields["4jcknnmq"]

    @_4jcknnmq.setter
    def _4jcknnmq(self, value: str) -> None:
        self.edit("4jcknnmq", value=value)
    @property
    def _23s6djq3(self) -> bool:
        """skip (`23s6djq3` on the wire)."""
        return self.fields["23s6djq3"]

    @_23s6djq3.setter
    def _23s6djq3(self, value: bool) -> None:
        self.edit("23s6djq3", value=value)


class Vault(Resource):
    """Vault — `tl.so.tilo.passwords.Vault`."""
    __surface__ = "so.tilo.passwords@dac1b67d7a707940"
    vflzrccj: list[Entry[Item]]  # items
    @property
    def _6n7bahst(self) -> list[str]:
        """converted (`6n7bahst` on the wire)."""
        return self.fields["6n7bahst"]

    @_6n7bahst.setter
    def _6n7bahst(self, value: list[str]) -> None:
        self.edit("6n7bahst", value=value)
    wa4uxojt: list[Entry[ImportRow]]  # pending
    fatb5kmf: str  # importing
    @property
    def _3xf6uda7(self) -> str:
        """notice (`3xf6uda7` on the wire)."""
        return self.fields["3xf6uda7"]

    @_3xf6uda7.setter
    def _3xf6uda7(self, value: str) -> None:
        self.edit("3xf6uda7", value=value)

    def new_item(self,
        *,
        title: str | None = None,
    ) -> Item:
        """Starts an item: an entry of the vault and the item that lists it, named by what is typed first. Its secret is typed on the system's card, never here."""
        args: dict[str, Any] = {}
        if title is not None:
            args["title"] = title
        return self.action("new_item", **args)

    def save_login(self,
        *,
        name: str,
        origin: str,
        username: str,
    ) -> Item:
        """Adds a login in one step: what an agent, a script or the VM's own end-to-end check asks of the vault. The person types its password on the system's card, so whoever asked never holds it, and the vault keeps it. An address is checked as a web address."""
        return self.action("save_login", name=name, origin=origin, username=username)

    def list_logins(self) -> LoginList:
        """The vault's logins, by name and by the entry that holds each: what a script, an agent or another device's window lists. The passwords are never part of it."""
        return self.action("list_logins")

    def delete(self, *, item: Item) -> None:
        """Deletes an item, and the entry it stands for, from the vault."""
        return self.action("delete", item=item)

    def delete_many(self, items: list[DeleteArgs]) -> list[dict[str, Any]]:
        """Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete", items)

    def settle(self) -> None:
        """Brings the vault and its items together: an entry kept elsewhere (a mail account's password, a connector's token, one another device added) gets an item, an item's name, kind and tags reach its entry, and what an older Passwords kept in its own records moves into the vault and leaves the record. Runs while Passwords is open, and changes nothing when all is settled."""
        return self.action("settle")

    def settle_many(self, items: list[SettleArgs]) -> list[dict[str, Any]]:
        """Runs `settle` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("settle", items)

    def preview_csv(self, *, file: Any) -> None:
        """Reads the browser's CSV the person chose into the vault's waiting rows, each marked when the vault already holds that login. The passwords stay in the file."""
        return self.action("preview_csv", file=file)

    def preview_csv_many(self, items: list[PreviewCsvArgs]) -> list[dict[str, Any]]:
        """Runs `preview_csv` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("preview_csv", items)

    def apply_import(self) -> None:
        """Brings the rows the person agreed to into the vault, skipping the ones the vault holds. The vault reads their passwords from the file."""
        return self.action("apply_import")

    def apply_import_many(self, items: list[ApplyImportArgs]) -> list[dict[str, Any]]:
        """Runs `apply_import` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("apply_import", items)

    def clear_import(self) -> None:
        """Puts the waiting rows away."""
        return self.action("clear_import")

    def clear_import_many(self, items: list[ClearImportArgs]) -> list[dict[str, Any]]:
        """Runs `clear_import` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_import", items)

    def clear_pending(self) -> bool:
        """Empties the vault's waiting rows."""
        return self.action("clear_pending")

    def export_csv(self) -> Any:
        """The vault as a browser's CSV: the person chooses where it goes."""
        return self.action("export_csv")

    def export_backup(self) -> Any:
        """The vault as an encrypted backup under the password the person types twice on the system's card: the person chooses where it goes."""
        return self.action("export_backup")

    def restore_backup(self, *, file: Any) -> int:
        """Brings the entries a backup holds into the vault with the password the person types on the system's card, and answers how many came in. An entry the vault already keeps is left as it is, so restoring twice adds nothing."""
        return self.action("restore_backup", file=file)

    def absorb_device(self) -> None:
        """Takes into the vault what this device's secret service holds and a workspace may share. The device's authentication material and its Wi-Fi networks stay where they are, and each entry leaves the device's service once the vault keeps it."""
        return self.action("absorb_device")

    def absorb_device_many(self, items: list[AbsorbDeviceArgs]) -> list[dict[str, Any]]:
        """Runs `absorb_device` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("absorb_device", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_item", "save_login", "list_logins", "delete", "delete_many", "settle", "settle_many", "preview_csv", "preview_csv_many", "apply_import", "apply_import_many", "clear_import", "clear_import_many", "clear_pending", "export_csv", "export_backup", "restore_backup", "absorb_device", "absorb_device_many"})


class LoginList(Resource):
    """LoginList — `tl.so.tilo.passwords.LoginList`."""
    __surface__ = "so.tilo.passwords@dac1b67d7a707940"
    nkbtq6y2: list[LoginEntry]  # logins


class LoginEntry(Resource):
    """LoginEntry — `tl.so.tilo.passwords.LoginEntry`."""
    __surface__ = "so.tilo.passwords@dac1b67d7a707940"
    mtdotryg: str  # item
    yiseqxtu: str  # name


class TypedAsk(Resource):
    """TypedAsk — `tl.so.tilo.passwords.TypedAsk`."""
    __surface__ = "so.tilo.passwords@dac1b67d7a707940"
    @property
    def _5se7svg2(self) -> str:
        """id (`5se7svg2` on the wire)."""
        return self.fields["5se7svg2"]

    @_5se7svg2.setter
    def _5se7svg2(self, value: str) -> None:
        self.edit("5se7svg2", value=value)
    @property
    def _3f2i24zq(self) -> str:
        """part (`3f2i24zq` on the wire)."""
        return self.fields["3f2i24zq"]

    @_3f2i24zq.setter
    def _3f2i24zq(self, value: str) -> None:
        self.edit("3f2i24zq", value=value)


class AskedFile(Resource):
    """AskedFile — `tl.so.tilo.passwords.AskedFile`."""
    __surface__ = "so.tilo.passwords@dac1b67d7a707940"
    roquvgtj: str  # url


class ImportAsk(Resource):
    """ImportAsk — `tl.so.tilo.passwords.ImportAsk`."""
    __surface__ = "so.tilo.passwords@dac1b67d7a707940"
    trzw7c4z: str  # url
    xbct2xaa: list[int]  # skip


class CsvRow(Resource):
    """CsvRow — `tl.so.tilo.passwords.CsvRow`."""
    __surface__ = "so.tilo.passwords@dac1b67d7a707940"
    ufq2o5qa: str  # title
    pjl3fx3t: str  # username
    @property
    def _7ca4ejcb(self) -> str:
        """origin (`7ca4ejcb` on the wire)."""
        return self.fields["7ca4ejcb"]

    @_7ca4ejcb.setter
    def _7ca4ejcb(self, value: str) -> None:
        self.edit("7ca4ejcb", value=value)


class RowsAnswer(Resource):
    """RowsAnswer — `tl.so.tilo.passwords.RowsAnswer`."""
    __surface__ = "so.tilo.passwords@dac1b67d7a707940"
    janzxbln: list[CsvRow]  # rows


class CountAnswer(Resource):
    """CountAnswer — `tl.so.tilo.passwords.CountAnswer`."""
    __surface__ = "so.tilo.passwords@dac1b67d7a707940"
    @property
    def _35klc2sc(self) -> int:
        """imported (`35klc2sc` on the wire)."""
        return self.fields["35klc2sc"]

    @_35klc2sc.setter
    def _35klc2sc(self, value: int) -> None:
        self.edit("35klc2sc", value=value)


class RestoredAnswer(Resource):
    """RestoredAnswer — `tl.so.tilo.passwords.RestoredAnswer`."""
    __surface__ = "so.tilo.passwords@dac1b67d7a707940"
    ks2ix353: int  # restored


class CarriedAnswer(Resource):
    """CarriedAnswer — `tl.so.tilo.passwords.CarriedAnswer`."""
    __surface__ = "so.tilo.passwords@dac1b67d7a707940"
    wd5mzler: int  # carried
    arw4q74m: str  # error


class UrlAnswer(Resource):
    """UrlAnswer — `tl.so.tilo.passwords.UrlAnswer`."""
    __surface__ = "so.tilo.passwords@dac1b67d7a707940"
    vlz4ie4r: str  # url


class Passwords(Resource):
    """Passwords — `tl.so.tilo.passwords`."""
    __surface__ = "so.tilo.passwords@dac1b67d7a707940"
    def keep_typed(self,
        *,
        id: str,
        part: str,
    ) -> str:
        """Hands what the person typed on the system's card to the vault as `part` of the entry `id`: the host passes it on, so it never becomes a value here. Answers why it could not, or nothing."""
        return self.action("keep_typed", id=id, part=part)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"keep_typed"})


