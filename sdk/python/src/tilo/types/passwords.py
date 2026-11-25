"""Types for Passwords (`so.tilo.passwords`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Notice` is the app's `tl.tilo.host.Notice` at `passwords/<resource>`, read with
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
    "Notice",
    "Item",
    "ImportRow",
    "Vault",
    "LoginList",
    "LoginEntry",
    "Shown",
    "ShownField",
    "Access",
    "Use",
    "Leave",
    "TypedAsk",
    "TokenAnswer",
    "AskedFile",
    "ImportAsk",
    "CsvRow",
    "RowsAnswer",
    "HeldItem",
    "HeldField",
    "HeldItems",
    "DeviceItems",
    "DeviceRemove",
    "ToFile",
    "UrlAnswer",
    "Passwords",
    "LearnArgs",
    "FavoriteArgs",
    "PreviewCsvArgs",
    "ClearImportArgs",
]

Kind = Literal["login", "api_key", "note", "wifi", "secret"]
"""What kind of secret an item is: what it is for, which sets the fields it has and the words that name them."""

LearnArgs = TypedDict("LearnArgs", {
    "listed": "Any",
})
"""The arguments of `learn`."""

FavoriteArgs = TypedDict("FavoriteArgs", {
    "favorite": NotRequired["bool | None"],
})
"""The arguments of `favorite`."""

PreviewCsvArgs = TypedDict("PreviewCsvArgs", {
    "file": "Any",
})
"""The arguments of `preview_csv`."""

ClearImportArgs = TypedDict("ClearImportArgs", {
})
"""The arguments of `clear_import`."""

class Notice(Resource):
    """Notice — `tl.tilo.host.Notice`."""
    __surface__ = "so.tilo.passwords@30c76bd7d3a3aa8a"
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


class Item(Resource):
    """Item — `tl.so.tilo.passwords.Item`."""
    __surface__ = "so.tilo.passwords@30c76bd7d3a3aa8a"
    vvthph26: str  # entry
    ildzcyo2: str  # title
    ujkdowq4: Kind  # kind
    qoa35hei: list[str]  # tags
    tzeujwn7: bool  # favorite
    zpn4vzos: list[str]  # places
    ksrrxwee: list[str]  # labels
    fht5aqwz: bool  # noted
    hy77vohr: int  # changed
    hzewwvyj: list[str]  # concerns
    tpqopgvl: str  # username
    txdg33mm: str  # password
    fxvfkz3x: str  # notes
    qgllbsrc: list[str]  # origins
    japk242o: dict[str, str]  # fields

    def learn(self, *, listed: Any) -> None:
        """Writes what the vault says of an item (`listed`, one of what it lists) into its record, where it differs: its name, kind and tags, where it signs in, its fields' labels, whether it keeps a note, when it changed and what is wrong with it."""
        return self.action("learn", listed=listed)

    def learn_many(self, items: list[LearnArgs]) -> list[dict[str, Any]]:
        """Runs `learn` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("learn", items)

    def keep(self, *, ask: str) -> str:
        """Asks the vault to keep `ask` (its add, as JSON) and writes what it answers into `item`. Answers why it could not, or nothing."""
        return self.action("keep", ask=ask)

    def rewrite(self,
        *,
        title: str,
        kind: Kind,
        places: list[str],
        more: list[str],
    ) -> str:
        """Writes the item as it is, with `title`, `kind` and `places` in place of its own and `more` beside them. Answers why it could not, or nothing."""
        return self.action("rewrite", title=title, kind=kind, places=places, more=more)

    def set_username(self, *, username: str) -> str:
        """Writes who the item signs in as."""
        return self.action("set_username", username=username)

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
    ) -> str:
        """Draws a strong password for the login where the vault is: 20 characters by default, with a capital, a small letter, a digit and a symbol. Nobody types or sees it."""
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
    ) -> str:
        """Gives an item a tag, as the words beside its tags are typed."""
        args: dict[str, Any] = {}
        if tag is not None:
            args["tag"] = tag
        return self.action("add_tag", **args)

    def remove_tag(self, *, tag: str) -> str:
        """Takes a tag off an item."""
        return self.action("remove_tag", tag=tag)

    def add_origin(self,
        *,
        origin: str | None = None,
    ) -> str:
        """Gives an item an address it signs in to, checked as a web address, as typed beside them."""
        args: dict[str, Any] = {}
        if origin is not None:
            args["origin"] = origin
        return self.action("add_origin", **args)

    def remove_origin(self, *, origin: str) -> str:
        """Takes an address off an item."""
        return self.action("remove_origin", origin=origin)

    def rename(self, *, title: str) -> str:
        """Renames an item, in its record and in the vault."""
        return self.action("rename", title=title)

    def set_kind(self, *, kind: Kind) -> str:
        """Changes what kind of secret an item is."""
        return self.action("set_kind", kind=kind)

    def copy_secret(self) -> str:
        """Puts the item's secret on the person's clipboard from the vault, which forgets it again after a while. It never passes through Passwords, and the vault records the copy as a use."""
        return self.action("copy_secret")

    def copy_identity(self) -> str:
        """Puts what identifies the person to the account on their clipboard from the vault: a login's username, an API key's account."""
        return self.action("copy_identity")

    def copy_part(self, *, part: str) -> str:
        """Asks the vault to put `part` of the item on the person's clipboard. Answers why it could not, or nothing."""
        return self.action("copy_part", part=part)

    def copy_origins(self) -> None:
        """Copies the addresses the item signs in to, one a line."""
        return self.action("copy_origins")

    def shown_part(self,
        *,
        part: str,
        how: str,
    ) -> str:
        """Asks the vault to show `part` of the item for a while, where `how` says (`show` for the window, `hold` for the host): the name it is shown under, or nothing where the item keeps none. The vault records each showing as a use."""
        return self.action("shown_part", part=part, how=how)

    def reveal(self) -> Shown:
        """Shows the item's secret, who it signs in as, its note and its fields in the window, for 30 seconds at most: each part by a name only the window reads it under."""
        return self.action("reveal")

    def access(self) -> Access:
        """Reads who used the item, and the leaves its person gave: what the item's access page shows."""
        return self.action("access")

    def take_back(self, *, leave: Leave) -> Access:
        """Takes back a leave the item's person gave: the agent or app it names asks again next time."""
        return self.action("take_back", leave=leave)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"learn", "learn_many", "keep", "rewrite", "set_username", "change_secret", "write_note", "add_field", "remove_field", "generate_password", "favorite", "favorite_many", "add_tag", "remove_tag", "add_origin", "remove_origin", "rename", "set_kind", "copy_secret", "copy_identity", "copy_part", "copy_origins", "shown_part", "reveal", "access", "take_back"})


class ImportRow(Resource):
    """ImportRow — `tl.so.tilo.passwords.ImportRow`."""
    __surface__ = "so.tilo.passwords@30c76bd7d3a3aa8a"
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
    __surface__ = "so.tilo.passwords@30c76bd7d3a3aa8a"
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

    def add_new(self, *, ask: str) -> Item | None:
        """Adds a new item to the vault from `ask` (its add, as JSON) and lists it: the item, or nothing where the vault did not keep it, with why in the vault's notice."""
        return self.action("add_new", ask=ask)

    def new_item(self,
        *,
        title: str | None = None,
    ) -> Item | None:
        """Starts a login: an item of the vault named by what is typed first, with a strong password drawn where the vault is, which nobody types or sees. The person changes it on the item."""
        args: dict[str, Any] = {}
        if title is not None:
            args["title"] = title
        return self.action("new_item", **args)

    def save_login(self,
        *,
        name: str,
        origin: str,
        username: str,
    ) -> Item | None:
        """Adds a login in one step: what an agent, a script or the VM's own end-to-end check asks of the vault. The person types its password on the system's card, so whoever asked never holds it, and the vault keeps it. An address is checked as a web address."""
        return self.action("save_login", name=name, origin=origin, username=username)

    def list_logins(self) -> LoginList:
        """The vault's logins, by name and by the item that holds each: what a script, an agent or another device's window lists. The passwords are never part of it."""
        return self.action("list_logins")

    def delete(self, *, item: Item) -> None:
        """Deletes an item from the vault."""
        return self.action("delete", item=item)

    def settle(self) -> None:
        """Brings the vault and its items together: an item the vault keeps that is not listed (a mail account's password, a connector's token, one another device added) gets a record, what the vault says of each listed one reaches its record, one the vault no longer keeps leaves the list, and what an older Passwords kept in its own records moves into the vault and leaves the record. Runs while Passwords is open, and changes nothing when all is settled."""
        return self.action("settle")

    def carry(self, *, item: Item) -> None:
        """Moves what an item's record keeps into the vault, once, and empties the record. A record that names an item the vault keeps changes that item; one that names none adds one."""
        return self.action("carry", item=item)

    def preview_csv(self, *, file: Any) -> None:
        """Reads the browser's CSV the person chose into the vault's waiting rows, each marked when the vault already holds that login. The passwords stay in the file."""
        return self.action("preview_csv", file=file)

    def preview_csv_many(self, items: list[PreviewCsvArgs]) -> list[dict[str, Any]]:
        """Runs `preview_csv` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("preview_csv", items)

    def add_read(self, *, item: HeldItem) -> bool:
        """Adds an item the host read from a file the person chose: `item`, its secrets each named by the token the host holds it under. Answers whether the vault kept it."""
        return self.action("add_read", item=item)

    def apply_import(self) -> None:
        """Brings the rows the person agreed to into the vault, skipping the ones the vault holds. The host reads their passwords from the file and the vault takes them from the host."""
        return self.action("apply_import")

    def clear_import(self) -> None:
        """Puts the waiting rows away."""
        return self.action("clear_import")

    def clear_import_many(self, items: list[ClearImportArgs]) -> list[dict[str, Any]]:
        """Runs `clear_import` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_import", items)

    def clear_pending(self) -> bool:
        """Empties the vault's waiting rows."""
        return self.action("clear_pending")

    def held_items(self) -> list[HeldItem]:
        """Every listed item with its secrets, each named by the token the host holds it under: what the host writes into a file the person chose. The vault records each part handed over as a use."""
        return self.action("held_items")

    def export_csv(self) -> Any:
        """The vault as a browser's CSV: the person chooses where it goes."""
        return self.action("export_csv")

    def export_backup(self) -> Any:
        """The vault as an encrypted backup under the password the person types twice on the system's card: the person chooses where it goes."""
        return self.action("export_backup")

    def restore_backup(self, *, file: Any) -> int:
        """Brings the entries a backup holds into the vault with the password the person types on the system's card, and answers how many came in. An item the vault already lists under its name and address is left as it is, so restoring twice adds nothing."""
        return self.action("restore_backup", file=file)

    def absorb_device(self) -> None:
        """Takes into the vault what this device's secret service holds and a workspace may share. The device's authentication material and its Wi-Fi networks stay where they are, and each entry leaves the device's service once the vault keeps it."""
        return self.action("absorb_device")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"add_new", "new_item", "save_login", "list_logins", "delete", "settle", "carry", "preview_csv", "preview_csv_many", "add_read", "apply_import", "clear_import", "clear_import_many", "clear_pending", "held_items", "export_csv", "export_backup", "restore_backup", "absorb_device"})


class LoginList(Resource):
    """LoginList — `tl.so.tilo.passwords.LoginList`."""
    __surface__ = "so.tilo.passwords@30c76bd7d3a3aa8a"
    nkbtq6y2: list[LoginEntry]  # logins


class LoginEntry(Resource):
    """LoginEntry — `tl.so.tilo.passwords.LoginEntry`."""
    __surface__ = "so.tilo.passwords@30c76bd7d3a3aa8a"
    mtdotryg: str  # item
    yiseqxtu: str  # name


class Shown(Resource):
    """Shown — `tl.so.tilo.passwords.Shown`."""
    __surface__ = "so.tilo.passwords@30c76bd7d3a3aa8a"
    @property
    def _4ggtifo6(self) -> str:
        """secret (`4ggtifo6` on the wire)."""
        return self.fields["4ggtifo6"]

    @_4ggtifo6.setter
    def _4ggtifo6(self, value: str) -> None:
        self.edit("4ggtifo6", value=value)
    k2igcsy4: str  # identity
    kz5emarz: str  # notes
    bs66mwlz: list[ShownField]  # fields


class ShownField(Resource):
    """ShownField — `tl.so.tilo.passwords.ShownField`."""
    __surface__ = "so.tilo.passwords@30c76bd7d3a3aa8a"
    @property
    def _47ctn52x(self) -> str:
        """label (`47ctn52x` on the wire)."""
        return self.fields["47ctn52x"]

    @_47ctn52x.setter
    def _47ctn52x(self, value: str) -> None:
        self.edit("47ctn52x", value=value)
    @property
    def _72h5hcgw(self) -> str:
        """value (`72h5hcgw` on the wire)."""
        return self.fields["72h5hcgw"]

    @_72h5hcgw.setter
    def _72h5hcgw(self, value: str) -> None:
        self.edit("72h5hcgw", value=value)


class Access(Resource):
    """Access — `tl.so.tilo.passwords.Access`."""
    __surface__ = "so.tilo.passwords@30c76bd7d3a3aa8a"
    upwe4jla: list[Use]  # uses
    n52kczfg: list[Leave]  # leaves
    ll3dunru: str  # trouble


class Use(Resource):
    """Use — `tl.so.tilo.passwords.Use`."""
    __surface__ = "so.tilo.passwords@30c76bd7d3a3aa8a"
    dhbq7ocx: int  # at
    ldvya4gh: str  # by
    xsu4yq6o: str  # what
    ccefrfoc: str  # places


class Leave(Resource):
    """Leave — `tl.so.tilo.passwords.Leave`."""
    __surface__ = "so.tilo.passwords@30c76bd7d3a3aa8a"
    @property
    def _2emebh4y(self) -> str:
        """agent (`2emebh4y` on the wire)."""
        return self.fields["2emebh4y"]

    @_2emebh4y.setter
    def _2emebh4y(self, value: str) -> None:
        self.edit("2emebh4y", value=value)
    crfod54g: str  # for_app
    eqysm75s: str  # by
    rgwbpnf4: str  # using
    @property
    def _6ovlcphf(self) -> int:
        """since (`6ovlcphf` on the wire)."""
        return self.fields["6ovlcphf"]

    @_6ovlcphf.setter
    def _6ovlcphf(self, value: int) -> None:
        self.edit("6ovlcphf", value=value)


class TypedAsk(Resource):
    """TypedAsk — `tl.so.tilo.passwords.TypedAsk`."""
    __surface__ = "so.tilo.passwords@30c76bd7d3a3aa8a"
    upwiyhzz: int  # place


class TokenAnswer(Resource):
    """TokenAnswer — `tl.so.tilo.passwords.TokenAnswer`."""
    __surface__ = "so.tilo.passwords@30c76bd7d3a3aa8a"
    @property
    def _5t7dz6wt(self) -> str:
        """token (`5t7dz6wt` on the wire)."""
        return self.fields["5t7dz6wt"]

    @_5t7dz6wt.setter
    def _5t7dz6wt(self, value: str) -> None:
        self.edit("5t7dz6wt", value=value)


class AskedFile(Resource):
    """AskedFile — `tl.so.tilo.passwords.AskedFile`."""
    __surface__ = "so.tilo.passwords@30c76bd7d3a3aa8a"
    roquvgtj: str  # url


class ImportAsk(Resource):
    """ImportAsk — `tl.so.tilo.passwords.ImportAsk`."""
    __surface__ = "so.tilo.passwords@30c76bd7d3a3aa8a"
    trzw7c4z: str  # url
    xbct2xaa: list[int]  # skip


class CsvRow(Resource):
    """CsvRow — `tl.so.tilo.passwords.CsvRow`."""
    __surface__ = "so.tilo.passwords@30c76bd7d3a3aa8a"
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
    __surface__ = "so.tilo.passwords@30c76bd7d3a3aa8a"
    janzxbln: list[CsvRow]  # rows


class HeldItem(Resource):
    """HeldItem — `tl.so.tilo.passwords.HeldItem`."""
    __surface__ = "so.tilo.passwords@30c76bd7d3a3aa8a"
    @property
    def _54afy6gc(self) -> str:
        """id (`54afy6gc` on the wire)."""
        return self.fields["54afy6gc"]

    @_54afy6gc.setter
    def _54afy6gc(self, value: str) -> None:
        self.edit("54afy6gc", value=value)
    jtfqafog: str  # kind
    qct2yfix: str  # title
    pjgoev5i: str  # username
    vqux3uy5: list[str]  # origins
    pngdxwz4: list[str]  # tags
    @property
    def _3zfbij5u(self) -> str:
        """password (`3zfbij5u` on the wire)."""
        return self.fields["3zfbij5u"]

    @_3zfbij5u.setter
    def _3zfbij5u(self, value: str) -> None:
        self.edit("3zfbij5u", value=value)
    k2xr3in6: str  # notes
    s5jm3xad: list[HeldField]  # fields


class HeldField(Resource):
    """HeldField — `tl.so.tilo.passwords.HeldField`."""
    __surface__ = "so.tilo.passwords@30c76bd7d3a3aa8a"
    b7ymmw3n: str  # label
    wgkjnofz: str  # value


class HeldItems(Resource):
    """HeldItems — `tl.so.tilo.passwords.HeldItems`."""
    __surface__ = "so.tilo.passwords@30c76bd7d3a3aa8a"
    awioulwr: list[HeldItem]  # items


class DeviceItems(Resource):
    """DeviceItems — `tl.so.tilo.passwords.DeviceItems`."""
    __surface__ = "so.tilo.passwords@30c76bd7d3a3aa8a"
    ghk43anz: list[HeldItem]  # items
    lzu63qlu: str  # error


class DeviceRemove(Resource):
    """DeviceRemove — `tl.so.tilo.passwords.DeviceRemove`."""
    __surface__ = "so.tilo.passwords@30c76bd7d3a3aa8a"
    tzeqieyr: list[str]  # ids


class ToFile(Resource):
    """ToFile — `tl.so.tilo.passwords.ToFile`."""
    __surface__ = "so.tilo.passwords@30c76bd7d3a3aa8a"
    @property
    def _7eqpvzfc(self) -> list[HeldItem]:
        """items (`7eqpvzfc` on the wire)."""
        return self.fields["7eqpvzfc"]

    @_7eqpvzfc.setter
    def _7eqpvzfc(self, value: list[HeldItem]) -> None:
        self.edit("7eqpvzfc", value=value)
    kfrmuqfw: str  # passphrase


class UrlAnswer(Resource):
    """UrlAnswer — `tl.so.tilo.passwords.UrlAnswer`."""
    __surface__ = "so.tilo.passwords@30c76bd7d3a3aa8a"
    vlz4ie4r: str  # url


class Passwords(Resource):
    """Passwords — `tl.so.tilo.passwords`."""
    __surface__ = "so.tilo.passwords@30c76bd7d3a3aa8a"

