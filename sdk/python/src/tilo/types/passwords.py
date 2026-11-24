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
    "AskedItem",
    "ItemsAsk",
    "BackupAsk",
    "RestoreAsk",
    "AskedFile",
    "RemoveAsk",
    "DeviceItem",
    "DeviceAnswer",
    "CsvRow",
    "RowsAnswer",
    "UrlAnswer",
    "Passwords",
    "SaveEditorArgs",
    "GeneratePasswordArgs",
    "FavoriteArgs",
    "AddTagArgs",
    "RemoveTagArgs",
    "AddOriginArgs",
    "RemoveOriginArgs",
    "NoticeAgeArgs",
    "DeleteArgs",
    "PreviewCsvArgs",
    "ApplyImportArgs",
    "ClearImportArgs",
    "AbsorbDeviceArgs",
]

Kind = Literal["login", "api_key", "note", "wifi", "secret"]
"""What kind of secret an item is: what it is for, which sets the fields it has and the words that name them."""

SaveEditorArgs = TypedDict("SaveEditorArgs", {
    "password": "str",
    "notes": "str",
    "fields": "str",
})
"""The arguments of `save_editor`."""

GeneratePasswordArgs = TypedDict("GeneratePasswordArgs", {
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

NoticeAgeArgs = TypedDict("NoticeAgeArgs", {
})
"""The arguments of `notice_age`."""

DeleteArgs = TypedDict("DeleteArgs", {
    "item": "Item",
})
"""The arguments of `delete`."""

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
    __surface__ = "so.tilo.passwords@37130bd8058a96d7"
    ildzcyo2: str  # title
    ujkdowq4: Kind  # kind
    tpqopgvl: str  # username
    txdg33mm: str  # password
    fxvfkz3x: str  # notes
    qgllbsrc: list[str]  # origins
    qoa35hei: list[str]  # tags
    japk242o: dict[str, str]  # fields
    tzeujwn7: bool  # favorite
    qzpekkx6: bool  # old

    def save_editor(self,
        *,
        password: str,
        notes: str,
        fields: str,
    ) -> None:
        """Writes what the editor holds onto an item: the password and notes it was given, and the custom fields it typed. An empty password keeps the one the item had."""
        return self.action("save_editor", password=password, notes=notes, fields=fields)

    def save_editor_many(self, items: list[SaveEditorArgs]) -> list[dict[str, Any]]:
        """Runs `save_editor` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("save_editor", items)

    def generate_password(self) -> None:
        """Draws a strong password for the item: 20 characters, at least one capital, one small letter, one digit and one symbol."""
        return self.action("generate_password")

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

    def copy_secret(self) -> None:
        """Copies the item's secret to the person's clipboard."""
        return self.action("copy_secret")

    def copy_identity(self) -> None:
        """Copies what identifies the person to the account: a login's username, an API key's account, a Wi-Fi network's name."""
        return self.action("copy_identity")

    def copy_origins(self) -> None:
        """Copies the addresses the item signs in to, one a line."""
        return self.action("copy_origins")

    def notice_age(self) -> None:
        """Notes an item whose password was last changed more than a year ago, as the daily check runs."""
        return self.action("notice_age")

    def notice_age_many(self, items: list[NoticeAgeArgs]) -> list[dict[str, Any]]:
        """Runs `notice_age` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("notice_age", items)

    def write_origins(self, *, values: list[str]) -> bool:
        """Replaces an item's addresses with `values`, one change."""
        return self.action("write_origins", values=values)

    def write_tags(self, *, values: list[str]) -> bool:
        """Replaces an item's tags with `values`, one change."""
        return self.action("write_tags", values=values)

    def write_fields(self, *, values: dict[str, str]) -> bool:
        """Replaces an item's custom fields with `values`, one change."""
        return self.action("write_fields", values=values)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"save_editor", "save_editor_many", "generate_password", "generate_password_many", "favorite", "favorite_many", "add_tag", "add_tag_many", "remove_tag", "remove_tag_many", "add_origin", "add_origin_many", "remove_origin", "remove_origin_many", "copy_secret", "copy_identity", "copy_origins", "notice_age", "notice_age_many", "write_origins", "write_tags", "write_fields"})


class ImportRow(Resource):
    """ImportRow — `tl.so.tilo.passwords.ImportRow`."""
    __surface__ = "so.tilo.passwords@37130bd8058a96d7"
    l7jn4eol: str  # title
    s6pvezan: str  # username
    @property
    def _4jcknnmq(self) -> str:
        """origin (`4jcknnmq` on the wire)."""
        return self.fields["4jcknnmq"]

    @_4jcknnmq.setter
    def _4jcknnmq(self, value: str) -> None:
        self.edit("4jcknnmq", value=value)
    jtrf57qd: str  # password
    sx76etl2: str  # notes
    @property
    def _23s6djq3(self) -> bool:
        """skip (`23s6djq3` on the wire)."""
        return self.fields["23s6djq3"]

    @_23s6djq3.setter
    def _23s6djq3(self, value: bool) -> None:
        self.edit("23s6djq3", value=value)


class Vault(Resource):
    """Vault — `tl.so.tilo.passwords.Vault`."""
    __surface__ = "so.tilo.passwords@37130bd8058a96d7"
    vflzrccj: list[Entry[Item]]  # items
    @property
    def _6n7bahst(self) -> list[str]:
        """converted (`6n7bahst` on the wire)."""
        return self.fields["6n7bahst"]

    @_6n7bahst.setter
    def _6n7bahst(self, value: list[str]) -> None:
        self.edit("6n7bahst", value=value)
    wa4uxojt: list[Entry[ImportRow]]  # pending
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
        """Starts an item: it opens in the window, named by what is typed first."""
        args: dict[str, Any] = {}
        if title is not None:
            args["title"] = title
        return self.action("new_item", **args)

    def save_login(self,
        *,
        name: str,
        origin: str,
        username: str,
        password: str,
    ) -> Item:
        """Adds a login in one step: what an agent, a script or the VM's own end-to-end check asks of the vault. The name is required, the password is required, and an address is checked as a web address."""
        return self.action("save_login", name=name, origin=origin, username=username, password=password)

    def list_logins(self) -> LoginList:
        """The vault's logins, by name and by the item that holds each: what a script, an agent or another device's window lists. The passwords are never part of it."""
        return self.action("list_logins")

    def delete(self, *, item: Item) -> None:
        """Deletes an item from the vault."""
        return self.action("delete", item=item)

    def delete_many(self, items: list[DeleteArgs]) -> list[dict[str, Any]]:
        """Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete", items)

    def preview_csv(self, *, file: Any) -> None:
        """Reads the browser's CSV the person chose into the vault's waiting rows, each marked when the vault already holds that login."""
        return self.action("preview_csv", file=file)

    def preview_csv_many(self, items: list[PreviewCsvArgs]) -> list[dict[str, Any]]:
        """Runs `preview_csv` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("preview_csv", items)

    def apply_import(self) -> None:
        """Brings the rows the person agreed to into the vault, skipping the ones the vault holds."""
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
        """The vault as a browser's CSV, named for what it holds: the person chooses where it goes."""
        return self.action("export_csv")

    def export_backup(self, *, password: str) -> Any:
        """The vault as an encrypted backup under `password`: the person chooses where it goes."""
        return self.action("export_backup", password=password)

    def restore_backup(self,
        *,
        password: str,
        file: Any,
    ) -> int:
        """Brings the items a backup holds into the vault, and answers how many came in."""
        return self.action("restore_backup", password=password, file=file)

    def absorb_device(self) -> None:
        """Takes into the vault what this device's secret service holds and a workspace may share. Each entry is sealed into the vault; the device's authentication material and its Wi-Fi networks stay where they are. An entry already taken in is not taken again."""
        return self.action("absorb_device")

    def absorb_device_many(self, items: list[AbsorbDeviceArgs]) -> list[dict[str, Any]]:
        """Runs `absorb_device` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("absorb_device", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"new_item", "save_login", "list_logins", "delete", "delete_many", "preview_csv", "preview_csv_many", "apply_import", "apply_import_many", "clear_import", "clear_import_many", "clear_pending", "export_csv", "export_backup", "restore_backup", "absorb_device", "absorb_device_many"})


class LoginList(Resource):
    """LoginList — `tl.so.tilo.passwords.LoginList`."""
    __surface__ = "so.tilo.passwords@37130bd8058a96d7"
    nkbtq6y2: list[LoginEntry]  # logins


class LoginEntry(Resource):
    """LoginEntry — `tl.so.tilo.passwords.LoginEntry`."""
    __surface__ = "so.tilo.passwords@37130bd8058a96d7"
    mtdotryg: str  # item
    yiseqxtu: str  # name


class AskedItem(Resource):
    """AskedItem — `tl.so.tilo.passwords.AskedItem`."""
    __surface__ = "so.tilo.passwords@37130bd8058a96d7"
    rzuvlal6: str  # title
    @property
    def _65lzjom4(self) -> str:
        """kind (`65lzjom4` on the wire)."""
        return self.fields["65lzjom4"]

    @_65lzjom4.setter
    def _65lzjom4(self, value: str) -> None:
        self.edit("65lzjom4", value=value)
    gtezt3ys: str  # username
    nqzlkgho: str  # password
    nyoijvnx: str  # notes
    pupxu7gb: list[str]  # origins
    zrtsugqu: list[str]  # tags
    yczcbynv: dict[str, str]  # fields


class ItemsAsk(Resource):
    """ItemsAsk — `tl.so.tilo.passwords.ItemsAsk`."""
    __surface__ = "so.tilo.passwords@37130bd8058a96d7"
    @property
    def _6nicnbs2(self) -> list[AskedItem]:
        """items (`6nicnbs2` on the wire)."""
        return self.fields["6nicnbs2"]

    @_6nicnbs2.setter
    def _6nicnbs2(self, value: list[AskedItem]) -> None:
        self.edit("6nicnbs2", value=value)


class BackupAsk(Resource):
    """BackupAsk — `tl.so.tilo.passwords.BackupAsk`."""
    __surface__ = "so.tilo.passwords@37130bd8058a96d7"
    a45t5mnl: list[AskedItem]  # items
    wmhsd5ch: str  # passphrase


class RestoreAsk(Resource):
    """RestoreAsk — `tl.so.tilo.passwords.RestoreAsk`."""
    __surface__ = "so.tilo.passwords@37130bd8058a96d7"
    rnonlkte: str  # url
    cynjyelt: str  # passphrase


class AskedFile(Resource):
    """AskedFile — `tl.so.tilo.passwords.AskedFile`."""
    __surface__ = "so.tilo.passwords@37130bd8058a96d7"
    roquvgtj: str  # url


class RemoveAsk(Resource):
    """RemoveAsk — `tl.so.tilo.passwords.RemoveAsk`."""
    __surface__ = "so.tilo.passwords@37130bd8058a96d7"
    eevmellb: list[str]  # ids


class DeviceItem(Resource):
    """DeviceItem — `tl.so.tilo.passwords.DeviceItem`."""
    __surface__ = "so.tilo.passwords@37130bd8058a96d7"
    wnjsdr3d: str  # id
    lwvhidtr: str  # title
    vd3jnhgx: str  # kind
    f7f2gp2a: str  # username
    @property
    def _6okrlhly(self) -> str:
        """password (`6okrlhly` on the wire)."""
        return self.fields["6okrlhly"]

    @_6okrlhly.setter
    def _6okrlhly(self, value: str) -> None:
        self.edit("6okrlhly", value=value)
    p4viod5a: str  # notes
    @property
    def _4ejpoi2g(self) -> list[str]:
        """origins (`4ejpoi2g` on the wire)."""
        return self.fields["4ejpoi2g"]

    @_4ejpoi2g.setter
    def _4ejpoi2g(self, value: list[str]) -> None:
        self.edit("4ejpoi2g", value=value)
    wze5ieza: list[str]  # tags
    gr5iqwm5: dict[str, str]  # fields


class DeviceAnswer(Resource):
    """DeviceAnswer — `tl.so.tilo.passwords.DeviceAnswer`."""
    __surface__ = "so.tilo.passwords@37130bd8058a96d7"
    vqmxqaz7: list[DeviceItem]  # items
    is77ciid: str  # error


class CsvRow(Resource):
    """CsvRow — `tl.so.tilo.passwords.CsvRow`."""
    __surface__ = "so.tilo.passwords@37130bd8058a96d7"
    ufq2o5qa: str  # title
    pjl3fx3t: str  # username
    @property
    def _7ca4ejcb(self) -> str:
        """origin (`7ca4ejcb` on the wire)."""
        return self.fields["7ca4ejcb"]

    @_7ca4ejcb.setter
    def _7ca4ejcb(self, value: str) -> None:
        self.edit("7ca4ejcb", value=value)
    u3j7epmt: str  # password
    we5a77lw: str  # notes


class RowsAnswer(Resource):
    """RowsAnswer — `tl.so.tilo.passwords.RowsAnswer`."""
    __surface__ = "so.tilo.passwords@37130bd8058a96d7"
    janzxbln: list[CsvRow]  # rows


class UrlAnswer(Resource):
    """UrlAnswer — `tl.so.tilo.passwords.UrlAnswer`."""
    __surface__ = "so.tilo.passwords@37130bd8058a96d7"
    vlz4ie4r: str  # url


class Passwords(Resource):
    """Passwords — `tl.so.tilo.passwords`."""
    __surface__ = "so.tilo.passwords@37130bd8058a96d7"

