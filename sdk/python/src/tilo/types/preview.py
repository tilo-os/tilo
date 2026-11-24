"""Types for Discord (`so.tilo.discord.preview`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Discord` is the app's `tl.so.tilo.discord.preview.Discord` at `preview/<resource>`, read with
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
    "ChannelKind",
    "MappedKind",
    "OriginSide",
    "Ownership",
    "OverwriteTarget",
    "Discord",
    "DChannel",
    "Overwrite",
    "DRole",
    "DEmoji",
    "Mapping",
    "Link",
    "LinkOffer",
    "Connect",
    "ServerChoice",
    "ServerList",
    "SendOp",
    "OutAttachment",
    "EditOp",
    "DeleteOp",
    "ReactOp",
    "StartThreadOp",
    "CreateWebhookOp",
    "ChannelSpec",
    "DeleteChannelOp",
    "CategoryPlace",
    "ChannelPlace",
    "ArrangeOp",
    "RoleSpec",
    "DeleteRoleOp",
    "MemberRolesOp",
    "MemberOp",
    "OverwriteOp",
    "FetchAssetOp",
    "AnswerInteractionOp",
    "EmojiSpec",
    "DeleteEmojiOp",
    "Perform",
    "Accounts",
    "Start",
    "SignInAsk",
    "VoicePair",
    "Preview",
    "OpenSetupArgs",
    "CloseSetupArgs",
    "ClearProblemArgs",
    "BridgeServerArgs",
    "FindServersArgs",
    "ConnectArgs",
    "ObserveLayoutArgs",
    "MapArgs",
    "UnmapArgs",
    "BeginConnectArgs",
    "ConnectAccountArgs",
    "OfferLinkArgs",
    "OfferSignInArgs",
    "KeepOfferArgs",
    "CompleteLinkArgs",
    "SetSignInArgs",
    "RenameLinkArgs",
    "SetLinkPhotoArgs",
    "UnlinkArgs",
    "UnlinkMineArgs",
    "BlockPersonaArgs",
    "UnblockPersonaArgs",
    "RememberAvatarArgs",
    "FinishImportArgs",
    "SettleEmojiArgs",
    "ReportVoiceArgs",
]

ChannelKind = Literal["text", "voice", "category", "thread"]
"""What kind of place a Discord channel is."""

MappedKind = Literal["channel", "category", "thread", "message", "webhook", "role", "emoji"]
"""What a mapping joins."""

OriginSide = Literal["tilo", "discord"]
"""Which side a mapped object was made on."""

Ownership = Literal["importing", "adopted"]
"""Whether the server is still being brought in, or bridged both ways."""

OverwriteTarget = Literal["role", "member"]
"""Whose permission overwrite an op sets."""

OpenSetupArgs = TypedDict("OpenSetupArgs", {
})
"""The arguments of `open_setup`."""

CloseSetupArgs = TypedDict("CloseSetupArgs", {
})
"""The arguments of `close_setup`."""

ClearProblemArgs = TypedDict("ClearProblemArgs", {
})
"""The arguments of `clear_problem`."""

BridgeServerArgs = TypedDict("BridgeServerArgs", {
    "s": "ServerChoice",
    "token": "str",
})
"""The arguments of `bridge_server`."""

FindServersArgs = TypedDict("FindServersArgs", {
    "token": "str",
})
"""The arguments of `find_servers`."""

ConnectArgs = TypedDict("ConnectArgs", {
    "guild_id": "str",
    "name": "str",
    "only_channel": NotRequired["str | None"],
    "mirror_channels": NotRequired["bool | None"],
    "map_roles": NotRequired["bool | None"],
})
"""The arguments of `connect`."""

ObserveLayoutArgs = TypedDict("ObserveLayoutArgs", {
    "bot_user": "str",
    "channels": "list[DChannel]",
    "roles": "list[DRole]",
    "emojis": "list[DEmoji]",
})
"""The arguments of `observe_layout`."""

MapArgs = TypedDict("MapArgs", {
    "kind": "MappedKind",
    "tilo": "str",
    "discord": "str",
    "origin": NotRequired["OriginSide | None"],
    "persona": NotRequired["str | None"],
    "root": NotRequired["str | None"],
})
"""The arguments of `map`."""

UnmapArgs = TypedDict("UnmapArgs", {
    "kind": "MappedKind",
    "tilo": "str",
    "keep_apart": NotRequired["bool | None"],
})
"""The arguments of `unmap`."""

BeginConnectArgs = TypedDict("BeginConnectArgs", {
    "state_sha256": "str",
})
"""The arguments of `begin_connect`."""

ConnectAccountArgs = TypedDict("ConnectAccountArgs", {
})
"""The arguments of `connect_account`."""

OfferLinkArgs = TypedDict("OfferLinkArgs", {
    "check": "str",
    "discord_user": "str",
    "user_name": "str",
    "display_name": NotRequired["str | None"],
    "avatar": NotRequired["str | None"],
    "interaction": NotRequired["str | None"],
})
"""The arguments of `offer_link`."""

OfferSignInArgs = TypedDict("OfferSignInArgs", {
    "check": "str",
    "discord_user": "str",
    "user_name": "str",
    "state_sha256": "str",
    "display_name": NotRequired["str | None"],
    "avatar": NotRequired["str | None"],
})
"""The arguments of `offer_sign_in`."""

KeepOfferArgs = TypedDict("KeepOfferArgs", {
    "offered": "LinkOffer",
})
"""The arguments of `keep_offer`."""

CompleteLinkArgs = TypedDict("CompleteLinkArgs", {
    "proof": "str",
})
"""The arguments of `complete_link`."""

SetSignInArgs = TypedDict("SetSignInArgs", {
    "page": "str",
})
"""The arguments of `set_sign_in`."""

RenameLinkArgs = TypedDict("RenameLinkArgs", {
    "discord_user": "str",
    "user_name": "str",
    "display_name": NotRequired["str | None"],
    "avatar": NotRequired["str | None"],
})
"""The arguments of `rename_link`."""

SetLinkPhotoArgs = TypedDict("SetLinkPhotoArgs", {
    "discord_user": "str",
    "avatar": "str",
    "photo": "str",
})
"""The arguments of `set_link_photo`."""

UnlinkArgs = TypedDict("UnlinkArgs", {
    "discord_user": "str",
})
"""The arguments of `unlink`."""

UnlinkMineArgs = TypedDict("UnlinkMineArgs", {
})
"""The arguments of `unlink_mine`."""

BlockPersonaArgs = TypedDict("BlockPersonaArgs", {
    "discord_user": "str",
})
"""The arguments of `block_persona`."""

UnblockPersonaArgs = TypedDict("UnblockPersonaArgs", {
    "discord_user": "str",
})
"""The arguments of `unblock_persona`."""

RememberAvatarArgs = TypedDict("RememberAvatarArgs", {
    "hash": "str",
    "blob": "str",
})
"""The arguments of `remember_avatar`."""

FinishImportArgs = TypedDict("FinishImportArgs", {
    "tilo_only": "list[str]",
})
"""The arguments of `finish_import`."""

SettleEmojiArgs = TypedDict("SettleEmojiArgs", {
    "tilo": "str",
    "emoji_id": NotRequired["str | None"],
    "problem": NotRequired["str | None"],
    "picture": NotRequired["str | None"],
    "was": NotRequired["str | None"],
    "deleted": NotRequired["bool | None"],
})
"""The arguments of `settle_emoji`."""

ReportVoiceArgs = TypedDict("ReportVoiceArgs", {
    "problem": "str",
})
"""The arguments of `report_voice`."""

class Discord(Resource):
    """Discord — `tl.so.tilo.discord.preview.Discord`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    sclfytrh: str  # name
    lex22ami: str  # guild_id
    @property
    def _2mt4scyz(self) -> str:
        """server (`2mt4scyz` on the wire)."""
        return self.fields["2mt4scyz"]

    @_2mt4scyz.setter
    def _2mt4scyz(self, value: str) -> None:
        self.edit("2mt4scyz", value=value)
    zoqgfxmk: str  # only_channel
    sqlamhjd: bool  # mirror_channels
    bxzjsd2v: bool  # map_roles
    jf2mmurb: Ownership  # ownership
    @property
    def _6hprgb4l(self) -> str:
        """token_item (`6hprgb4l` on the wire)."""
        return self.fields["6hprgb4l"]

    @_6hprgb4l.setter
    def _6hprgb4l(self, value: str) -> None:
        self.edit("6hprgb4l", value=value)
    k3mrdk3z: str  # bot_user
    pgejogzq: list[Entry[DChannel]]  # channels
    zhmdub7y: list[Entry[DRole]]  # roles
    aphz34n7: list[Entry[DEmoji]]  # emojis
    ma5o2ina: list[Entry[Mapping]]  # map
    ec3vplzb: list[Entry[Link]]  # links
    pf7f3cf2: list[Entry[LinkOffer]]  # link_offers
    kaj3zmhy: list[Entry[Connect]]  # connects
    db4wn64y: str  # sign_in_page
    bkxzddk4: str  # sign_in_url
    bdu3hrmn: dict[str, str]  # avatars
    @property
    def _6g22oebp(self) -> list[str]:
        """blocked_personas (`6g22oebp` on the wire)."""
        return self.fields["6g22oebp"]

    @_6g22oebp.setter
    def _6g22oebp(self, value: list[str]) -> None:
        self.edit("6g22oebp", value=value)
    y6utf74j: list[str]  # vanished
    htrhhc47: list[str]  # tilo_only
    b7dtt5ts: list[str]  # discord_only
    nn6mtwfm: str  # voice_problem
    flkdculv: str  # emoji_problem
    @property
    def _2vnxydnl(self) -> bool:
        """setup_open (`2vnxydnl` on the wire)."""
        return self.fields["2vnxydnl"]

    @_2vnxydnl.setter
    def _2vnxydnl(self, value: bool) -> None:
        self.edit("2vnxydnl", value=value)
    @property
    def _6fcfq4y6(self) -> list[Entry[ServerChoice]]:
        """setup_servers (`6fcfq4y6` on the wire)."""
        return self.fields["6fcfq4y6"]

    @_6fcfq4y6.setter
    def _6fcfq4y6(self, value: list[Entry[ServerChoice]]) -> None:
        self.edit("6fcfq4y6", value=value)
    pasjy4ku: str  # setup_problem

    def open_setup(self) -> None:
        """Runs the app's action."""
        return self.action("open_setup")

    def open_setup_many(self, items: list[OpenSetupArgs]) -> list[dict[str, Any]]:
        """Runs `open_setup` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("open_setup", items)

    def close_setup(self) -> None:
        """Closes it."""
        return self.action("close_setup")

    def close_setup_many(self, items: list[CloseSetupArgs]) -> list[dict[str, Any]]:
        """Runs `close_setup` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("close_setup", items)

    def clear_problem(self) -> None:
        """Forgets what the setup last said went wrong, as choosing a channel does."""
        return self.action("clear_problem")

    def clear_problem_many(self, items: list[ClearProblemArgs]) -> list[dict[str, Any]]:
        """Runs `clear_problem` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("clear_problem", items)

    def bridge_server(self,
        *,
        s: ServerChoice,
        token: str,
    ) -> None:
        """Binds this account to one server, with the token its bot uses. The server's layout stays Discord's until the import ran."""
        return self.action("bridge_server", s=s, token=token)

    def bridge_server_many(self, items: list[BridgeServerArgs]) -> list[dict[str, Any]]:
        """Runs `bridge_server` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("bridge_server", items)

    def find_servers(self, *, token: str) -> None:
        """Finds the servers the bot is in, so one can be chosen."""
        return self.action("find_servers", token=token)

    def find_servers_many(self, items: list[FindServersArgs]) -> list[dict[str, Any]]:
        """Runs `find_servers` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("find_servers", items)

    def connect(self,
        *,
        guild_id: str,
        name: str,
        only_channel: str | None = None,
        mirror_channels: bool | None = None,
        map_roles: bool | None = None,
    ) -> None:
        """Binds this account to one server. The server's layout stays Discord's until the import ran."""
        args: dict[str, Any] = {}
        args["guild_id"] = guild_id
        args["name"] = name
        if only_channel is not None:
            args["only_channel"] = only_channel
        if mirror_channels is not None:
            args["mirror_channels"] = mirror_channels
        if map_roles is not None:
            args["map_roles"] = map_roles
        return self.action("connect", **args)

    def connect_many(self, items: list[ConnectArgs]) -> list[dict[str, Any]]:
        """Runs `connect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("connect", items)

    def observe_layout(self,
        *,
        bot_user: str,
        channels: list[DChannel],
        roles: list[DRole],
        emojis: list[DEmoji],
    ) -> None:
        """Records Discord's layout as the latest event reported it: channels and roles the layout no longer lists go, the ones it lists are brought up to date, and new ones come in."""
        return self.action("observe_layout", bot_user=bot_user, channels=channels, roles=roles, emojis=emojis)

    def observe_layout_many(self, items: list[ObserveLayoutArgs]) -> list[dict[str, Any]]:
        """Runs `observe_layout` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("observe_layout", items)

    def map(self,
        *,
        kind: MappedKind,
        tilo: str,
        discord: str,
        origin: OriginSide | None = None,
        persona: str | None = None,
        root: str | None = None,
    ) -> None:
        """Joins a conversation, message or the like here to its Discord side."""
        args: dict[str, Any] = {}
        args["kind"] = kind
        args["tilo"] = tilo
        args["discord"] = discord
        if origin is not None:
            args["origin"] = origin
        if persona is not None:
            args["persona"] = persona
        if root is not None:
            args["root"] = root
        return self.action("map", **args)

    def map_many(self, items: list[MapArgs]) -> list[dict[str, Any]]:
        """Runs `map` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("map", items)

    def unmap(self,
        *,
        kind: MappedKind,
        tilo: str,
        keep_apart: bool | None = None,
    ) -> None:
        """Forgets a mapping, keeping the two sides apart from now on when asked."""
        args: dict[str, Any] = {}
        args["kind"] = kind
        args["tilo"] = tilo
        if keep_apart is not None:
            args["keep_apart"] = keep_apart
        return self.action("unmap", **args)

    def unmap_many(self, items: list[UnmapArgs]) -> list[dict[str, Any]]:
        """Runs `unmap` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("unmap", items)

    def begin_connect(self, *, state_sha256: str) -> None:
        """Runs the app's action."""
        return self.action("begin_connect", state_sha256=state_sha256)

    def begin_connect_many(self, items: list[BeginConnectArgs]) -> list[dict[str, Any]]:
        """Runs `begin_connect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("begin_connect", items)

    def connect_account(self) -> None:
        """Starts signing in with Discord, as the person looking: answers where to go."""
        return self.action("connect_account")

    def connect_account_many(self, items: list[ConnectAccountArgs]) -> list[dict[str, Any]]:
        """Runs `connect_account` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("connect_account", items)

    def offer_link(self,
        *,
        check: str,
        discord_user: str,
        user_name: str,
        display_name: str | None = None,
        avatar: str | None = None,
        interaction: str | None = None,
    ) -> None:
        """Offers a Discord account for linking, from the server: `/link` answered with a one-time token, whose check this is."""
        args: dict[str, Any] = {}
        args["check"] = check
        args["discord_user"] = discord_user
        args["user_name"] = user_name
        if display_name is not None:
            args["display_name"] = display_name
        if avatar is not None:
            args["avatar"] = avatar
        if interaction is not None:
            args["interaction"] = interaction
        return self.action("offer_link", **args)

    def offer_link_many(self, items: list[OfferLinkArgs]) -> list[dict[str, Any]]:
        """Runs `offer_link` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("offer_link", items)

    def offer_sign_in(self,
        *,
        check: str,
        discord_user: str,
        user_name: str,
        state_sha256: str,
        display_name: str | None = None,
        avatar: str | None = None,
    ) -> None:
        """Offers the account that signed in on Discord's consent page, to the person who started the round trip `state_sha256` names."""
        args: dict[str, Any] = {}
        args["check"] = check
        args["discord_user"] = discord_user
        args["user_name"] = user_name
        args["state_sha256"] = state_sha256
        if display_name is not None:
            args["display_name"] = display_name
        if avatar is not None:
            args["avatar"] = avatar
        return self.action("offer_sign_in", **args)

    def offer_sign_in_many(self, items: list[OfferSignInArgs]) -> list[dict[str, Any]]:
        """Runs `offer_sign_in` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("offer_sign_in", items)

    def keep_offer(self, *, offered: LinkOffer) -> None:
        """Keeps an offer for ten minutes, dropping those run out."""
        return self.action("keep_offer", offered=offered)

    def keep_offer_many(self, items: list[KeepOfferArgs]) -> list[dict[str, Any]]:
        """Runs `keep_offer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("keep_offer", items)

    def complete_link(self, *, proof: str) -> None:
        """Links the Discord account a one-time token offers to the person looking. The offer is used up, and a link the person had before is replaced."""
        return self.action("complete_link", proof=proof)

    def complete_link_many(self, items: list[CompleteLinkArgs]) -> list[dict[str, Any]]:
        """Runs `complete_link` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("complete_link", items)

    def set_sign_in(self, *, page: str) -> None:
        """Offers signing in with Discord: this device's consent page, without its state; empty for none."""
        return self.action("set_sign_in", page=page)

    def set_sign_in_many(self, items: list[SetSignInArgs]) -> list[dict[str, Any]]:
        """Runs `set_sign_in` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_sign_in", items)

    def answer_interaction(self, *, op: AnswerInteractionOp) -> None:
        """Edits a slash command's reply to what came of it."""
        return self.action("answer_interaction", op=op)

    def rename_link(self,
        *,
        discord_user: str,
        user_name: str,
        display_name: str | None = None,
        avatar: str | None = None,
    ) -> None:
        """Takes a linked account's newer names and avatar."""
        args: dict[str, Any] = {}
        args["discord_user"] = discord_user
        args["user_name"] = user_name
        if display_name is not None:
            args["display_name"] = display_name
        if avatar is not None:
            args["avatar"] = avatar
        return self.action("rename_link", **args)

    def rename_link_many(self, items: list[RenameLinkArgs]) -> list[dict[str, Any]]:
        """Runs `rename_link` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename_link", items)

    def set_link_photo(self,
        *,
        discord_user: str,
        avatar: str,
        photo: str,
    ) -> None:
        """Keeps a linked account's avatar as the person's picture, until they upload their own."""
        return self.action("set_link_photo", discord_user=discord_user, avatar=avatar, photo=photo)

    def set_link_photo_many(self, items: list[SetLinkPhotoArgs]) -> list[dict[str, Any]]:
        """Runs `set_link_photo` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set_link_photo", items)

    def unlink(self, *, discord_user: str) -> None:
        """Unlinks a Discord account: its person's, or the bridge's."""
        return self.action("unlink", discord_user=discord_user)

    def unlink_many(self, items: list[UnlinkArgs]) -> list[dict[str, Any]]:
        """Runs `unlink` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("unlink", items)

    def unlink_mine(self) -> None:
        """Unlinks the person's own Discord account."""
        return self.action("unlink_mine")

    def unlink_mine_many(self, items: list[UnlinkMineArgs]) -> list[dict[str, Any]]:
        """Runs `unlink_mine` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("unlink_mine", items)

    def block_persona(self, *, discord_user: str) -> None:
        """Stops mirroring a Discord account: its messages are no longer carried, and the ones already here are removed. It is not banned there."""
        return self.action("block_persona", discord_user=discord_user)

    def block_persona_many(self, items: list[BlockPersonaArgs]) -> list[dict[str, Any]]:
        """Runs `block_persona` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("block_persona", items)

    def unblock_persona(self, *, discord_user: str) -> None:
        """Mirrors a Discord account again."""
        return self.action("unblock_persona", discord_user=discord_user)

    def unblock_persona_many(self, items: list[UnblockPersonaArgs]) -> list[dict[str, Any]]:
        """Runs `unblock_persona` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("unblock_persona", items)

    def remember_avatar(self,
        *,
        hash: str,
        blob: str,
    ) -> None:
        """Remember where an avatar's image is stored."""
        return self.action("remember_avatar", hash=hash, blob=blob)

    def remember_avatar_many(self, items: list[RememberAvatarArgs]) -> list[dict[str, Any]]:
        """Runs `remember_avatar` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remember_avatar", items)

    def finish_import(self, *, tilo_only: list[str]) -> None:
        """Finishes adopting the server: everything in it is here now."""
        return self.action("finish_import", tilo_only=tilo_only)

    def finish_import_many(self, items: list[FinishImportArgs]) -> list[dict[str, Any]]:
        """Runs `finish_import` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("finish_import", items)

    def send_there(self, *, op: SendOp) -> None:
        """Sends a message through a channel's webhook, as the bridge plans it."""
        return self.action("send_there", op=op)

    def edit_there(self, *, op: EditOp) -> None:
        """Edits a message the bridge sent."""
        return self.action("edit_there", op=op)

    def delete_there(self, *, op: DeleteOp) -> None:
        """Deletes a message there."""
        return self.action("delete_there", op=op)

    def react_there(self, *, op: ReactOp) -> None:
        """Adds the bot's reaction."""
        return self.action("react_there", op=op)

    def unreact_there(self, *, op: ReactOp) -> None:
        """Takes the bot's reaction back."""
        return self.action("unreact_there", op=op)

    def start_thread_there(self, *, op: StartThreadOp) -> None:
        """Starts a thread on a message."""
        return self.action("start_thread_there", op=op)

    def create_webhook(self, *, op: CreateWebhookOp) -> None:
        """Creates a channel's webhook."""
        return self.action("create_webhook", op=op)

    def apply_channel(self, *, op: ChannelSpec) -> None:
        """Creates a channel there, or brings it back in line."""
        return self.action("apply_channel", op=op)

    def delete_channel_there(self, *, op: DeleteChannelOp) -> None:
        """Deletes a channel there."""
        return self.action("delete_channel_there", op=op)

    def arrange_channels(self, *, op: ArrangeOp) -> None:
        """Puts channels and categories in their places there, so the server lists them in this order."""
        return self.action("arrange_channels", op=op)

    def may_mirror(self) -> None:
        """Says whether the bridge may change the server's channels: mirroring is on and the server is adopted."""
        return self.action("may_mirror")

    def may_change(self, *, id: str) -> None:
        """Says whether the bridge made the Discord channel or category `id` for one here."""
        return self.action("may_change", id=id)

    def apply_role(self, *, op: RoleSpec) -> None:
        """Creates a role there, or brings it back in line."""
        return self.action("apply_role", op=op)

    def delete_role(self, *, op: DeleteRoleOp) -> None:
        """Deletes a role there."""
        return self.action("delete_role", op=op)

    def set_member_roles(self, *, op: MemberRolesOp) -> None:
        """Sets a linked member's roles there."""
        return self.action("set_member_roles", op=op)

    def kick(self, *, op: MemberOp) -> None:
        """Kicks a linked member there."""
        return self.action("kick", op=op)

    def ban(self, *, op: MemberOp) -> None:
        """Bans a linked member there."""
        return self.action("ban", op=op)

    def unban(self, *, op: MemberOp) -> None:
        """Unbans a linked member there."""
        return self.action("unban", op=op)

    def set_overwrite(self, *, op: OverwriteOp) -> None:
        """Sets a channel's permission overwrite, for a role the bridge made."""
        return self.action("set_overwrite", op=op)

    def fetch_asset(self, *, op: FetchAssetOp) -> None:
        """Copies a file from Discord's CDN into the workspace's content store."""
        return self.action("fetch_asset", op=op)

    def apply_emoji(self, *, op: EmojiSpec) -> None:
        """Uploads a workspace emoji to the server, or renames one the bridge uploaded."""
        return self.action("apply_emoji", op=op)

    def delete_emoji(self, *, op: DeleteEmojiOp) -> None:
        """Deletes an emoji the bridge uploaded."""
        return self.action("delete_emoji", op=op)

    def settle_emoji(self,
        *,
        tilo: str,
        emoji_id: str | None = None,
        problem: str | None = None,
        picture: str | None = None,
        was: str | None = None,
        deleted: bool | None = None,
    ) -> None:
        """Records what became of an emoji upload, rename or delete."""
        args: dict[str, Any] = {}
        args["tilo"] = tilo
        if emoji_id is not None:
            args["emoji_id"] = emoji_id
        if problem is not None:
            args["problem"] = problem
        if picture is not None:
            args["picture"] = picture
        if was is not None:
            args["was"] = was
        if deleted is not None:
            args["deleted"] = deleted
        return self.action("settle_emoji", **args)

    def settle_emoji_many(self, items: list[SettleEmojiArgs]) -> list[dict[str, Any]]:
        """Runs `settle_emoji` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("settle_emoji", items)

    def report_voice(self, *, problem: str) -> None:
        """Says why the bridge cannot carry voice now, or that it can again."""
        return self.action("report_voice", problem=problem)

    def report_voice_many(self, items: list[ReportVoiceArgs]) -> list[dict[str, Any]]:
        """Runs `report_voice` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("report_voice", items)

    def pump(self) -> None:
        """A value nothing reads, for the trigger's frame that does nothing itself: the origin polls Discord's events and the conversations here in its own loop."""
        return self.action("pump")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"open_setup", "open_setup_many", "close_setup", "close_setup_many", "clear_problem", "clear_problem_many", "bridge_server", "bridge_server_many", "find_servers", "find_servers_many", "connect", "connect_many", "observe_layout", "observe_layout_many", "map", "map_many", "unmap", "unmap_many", "begin_connect", "begin_connect_many", "connect_account", "connect_account_many", "offer_link", "offer_link_many", "offer_sign_in", "offer_sign_in_many", "keep_offer", "keep_offer_many", "complete_link", "complete_link_many", "set_sign_in", "set_sign_in_many", "answer_interaction", "rename_link", "rename_link_many", "set_link_photo", "set_link_photo_many", "unlink", "unlink_many", "unlink_mine", "unlink_mine_many", "block_persona", "block_persona_many", "unblock_persona", "unblock_persona_many", "remember_avatar", "remember_avatar_many", "finish_import", "finish_import_many", "send_there", "edit_there", "delete_there", "react_there", "unreact_there", "start_thread_there", "create_webhook", "apply_channel", "delete_channel_there", "arrange_channels", "may_mirror", "may_change", "apply_role", "delete_role", "set_member_roles", "kick", "ban", "unban", "set_overwrite", "fetch_asset", "apply_emoji", "delete_emoji", "settle_emoji", "settle_emoji_many", "report_voice", "report_voice_many", "pump"})


class DChannel(Resource):
    """DChannel — `tl.so.tilo.discord.preview.DChannel`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    @property
    def _7p2rou2g(self) -> str:
        """id (`7p2rou2g` on the wire)."""
        return self.fields["7p2rou2g"]

    @_7p2rou2g.setter
    def _7p2rou2g(self, value: str) -> None:
        self.edit("7p2rou2g", value=value)
    @property
    def _5g2es5cu(self) -> ChannelKind:
        """kind (`5g2es5cu` on the wire)."""
        return self.fields["5g2es5cu"]

    @_5g2es5cu.setter
    def _5g2es5cu(self, value: ChannelKind) -> None:
        self.edit("5g2es5cu", value=value)
    i2iwod32: str  # name
    kj47s5k3: str | None  # parent
    shpntyiy: int  # position
    ys7fs4ma: str  # topic
    @property
    def _3mwph4ce(self) -> list[Overwrite]:
        """overwrites (`3mwph4ce` on the wire)."""
        return self.fields["3mwph4ce"]

    @_3mwph4ce.setter
    def _3mwph4ce(self, value: list[Overwrite]) -> None:
        self.edit("3mwph4ce", value=value)


class Overwrite(Resource):
    """Overwrite — `tl.so.tilo.discord.preview.Overwrite`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    x427uoge: str  # id
    @property
    def _76fajf53(self) -> int:
        """kind (`76fajf53` on the wire)."""
        return self.fields["76fajf53"]

    @_76fajf53.setter
    def _76fajf53(self, value: int) -> None:
        self.edit("76fajf53", value=value)
    @property
    def _2yzg4vng(self) -> str:
        """allow (`2yzg4vng` on the wire)."""
        return self.fields["2yzg4vng"]

    @_2yzg4vng.setter
    def _2yzg4vng(self, value: str) -> None:
        self.edit("2yzg4vng", value=value)
    dbzgqxgr: str  # deny


class DRole(Resource):
    """DRole — `tl.so.tilo.discord.preview.DRole`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    xom5hhqp: str  # id
    aceuzwtj: str  # name
    wxuu6jgi: str  # permissions
    pddsqdvg: int  # color
    xtz54dig: int  # position
    zmedxuk3: bool  # managed


class DEmoji(Resource):
    """DEmoji — `tl.so.tilo.discord.preview.DEmoji`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    oaaqbeyg: str  # id
    @property
    def _5vadig7h(self) -> str:
        """name (`5vadig7h` on the wire)."""
        return self.fields["5vadig7h"]

    @_5vadig7h.setter
    def _5vadig7h(self, value: str) -> None:
        self.edit("5vadig7h", value=value)
    drktwmcm: bool  # animated


class Mapping(Resource):
    """Mapping — `tl.so.tilo.discord.preview.Mapping`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    ymr43fiv: MappedKind  # kind
    qt55olnf: str  # tilo
    vzp2s2xi: str  # discord
    tjidhwme: OriginSide  # origin
    ozmgf6kq: str  # persona
    @property
    def _4whazn6a(self) -> str:
        """root (`4whazn6a` on the wire)."""
        return self.fields["4whazn6a"]

    @_4whazn6a.setter
    def _4whazn6a(self, value: str) -> None:
        self.edit("4whazn6a", value=value)
    ctwuv3r6: str  # picture
    hj3vtcw2: str  # said
    j2bra6h6: list[str]  # emoji


class Link(Resource):
    """Link — `tl.so.tilo.discord.preview.Link`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    shknzkt7: str  # discord_user
    lvaegbi4: str  # person
    o4oyz2pi: str  # linked_at
    wivwsyri: str  # user_name
    qxx3lhen: str  # display_name
    i5lrpbng: str  # avatar
    e2cawszl: str  # interaction
    bbxol7y7: str  # photo
    fijk4xyy: str  # photo_avatar


class LinkOffer(Resource):
    """LinkOffer — `tl.so.tilo.discord.preview.LinkOffer`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    q4uc376y: str  # check
    @property
    def _6xuuyuym(self) -> str:
        """discord_user (`6xuuyuym` on the wire)."""
        return self.fields["6xuuyuym"]

    @_6xuuyuym.setter
    def _6xuuyuym(self, value: str) -> None:
        self.edit("6xuuyuym", value=value)
    @property
    def _3elvjryr(self) -> str:
        """user_name (`3elvjryr` on the wire)."""
        return self.fields["3elvjryr"]

    @_3elvjryr.setter
    def _3elvjryr(self, value: str) -> None:
        self.edit("3elvjryr", value=value)
    phqjglse: str  # display_name
    ynydwexj: str  # avatar
    am2zudro: str  # person
    umtbptha: str  # interaction
    lrq3epw3: str  # expires


class Connect(Resource):
    """Connect — `tl.so.tilo.discord.preview.Connect`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    @property
    def _3eaw5xx6(self) -> str:
        """person (`3eaw5xx6` on the wire)."""
        return self.fields["3eaw5xx6"]

    @_3eaw5xx6.setter
    def _3eaw5xx6(self, value: str) -> None:
        self.edit("3eaw5xx6", value=value)
    ljxvxizk: str  # state_sha256
    jpa253sd: str  # expires


class ServerChoice(Resource):
    """ServerChoice — `tl.so.tilo.discord.preview.ServerChoice`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    ymsxjog2: str  # id
    tafgiv6q: str  # name


class ServerList(Resource):
    """ServerList — `tl.so.tilo.discord.preview.ServerList`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    hj7lfaq7: list[ServerChoice]  # servers


class SendOp(Resource):
    """SendOp — `tl.so.tilo.discord.preview.SendOp`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    @property
    def _7lcsdnr4(self) -> str:
        """op (`7lcsdnr4` on the wire)."""
        return self.fields["7lcsdnr4"]

    @_7lcsdnr4.setter
    def _7lcsdnr4(self, value: str) -> None:
        self.edit("7lcsdnr4", value=value)
    amt4m7mw: str  # channel
    @property
    def _3dhjnhjd(self) -> str:
        """webhook (`3dhjnhjd` on the wire)."""
        return self.fields["3dhjnhjd"]

    @_3dhjnhjd.setter
    def _3dhjnhjd(self, value: str) -> None:
        self.edit("3dhjnhjd", value=value)
    heqsslfh: str | None  # thread
    fdgzp6on: str  # username
    qje4kbhs: str | None  # avatar_url
    nqgp6src: str | None  # avatar_blob
    d4x237pg: str  # content
    ffcddfvf: list[OutAttachment]  # attachments
    zhzpfsvd: int  # created_ms
    t3tk2ray: str  # tilo


class OutAttachment(Resource):
    """OutAttachment — `tl.so.tilo.discord.preview.OutAttachment`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    u7emqycf: str  # blob
    zs5m5uxa: str  # name
    aeq3264s: int  # size


class EditOp(Resource):
    """EditOp — `tl.so.tilo.discord.preview.EditOp`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    @property
    def _2ksixnn3(self) -> str:
        """op (`2ksixnn3` on the wire)."""
        return self.fields["2ksixnn3"]

    @_2ksixnn3.setter
    def _2ksixnn3(self, value: str) -> None:
        self.edit("2ksixnn3", value=value)
    @property
    def _2sbyakmv(self) -> str:
        """channel (`2sbyakmv` on the wire)."""
        return self.fields["2sbyakmv"]

    @_2sbyakmv.setter
    def _2sbyakmv(self, value: str) -> None:
        self.edit("2sbyakmv", value=value)
    bffu3pev: str  # webhook
    bfdmt6yv: str  # message
    jb6s4der: str | None  # thread
    raedlkm5: str  # content


class DeleteOp(Resource):
    """DeleteOp — `tl.so.tilo.discord.preview.DeleteOp`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    n5ulezfg: str  # op
    n2yclrmw: str  # channel
    liw2ay3q: str | None  # webhook
    w6a2lwlt: str  # message
    ojxxdcut: str | None  # thread


class ReactOp(Resource):
    """ReactOp — `tl.so.tilo.discord.preview.ReactOp`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    juyzi4a5: str  # op
    me76z7t7: str  # channel
    orzlvcjf: str  # message
    mhrysspc: str  # emoji


class StartThreadOp(Resource):
    """StartThreadOp — `tl.so.tilo.discord.preview.StartThreadOp`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    eqdu4s5n: str  # op
    qxyl2yhx: str  # channel
    @property
    def _2g4xqyvu(self) -> str:
        """message (`2g4xqyvu` on the wire)."""
        return self.fields["2g4xqyvu"]

    @_2g4xqyvu.setter
    def _2g4xqyvu(self, value: str) -> None:
        self.edit("2g4xqyvu", value=value)
    @property
    def _2ba4r76l(self) -> str:
        """name (`2ba4r76l` on the wire)."""
        return self.fields["2ba4r76l"]

    @_2ba4r76l.setter
    def _2ba4r76l(self, value: str) -> None:
        self.edit("2ba4r76l", value=value)
    @property
    def _63ujdmb5(self) -> str:
        """tilo (`63ujdmb5` on the wire)."""
        return self.fields["63ujdmb5"]

    @_63ujdmb5.setter
    def _63ujdmb5(self, value: str) -> None:
        self.edit("63ujdmb5", value=value)


class CreateWebhookOp(Resource):
    """CreateWebhookOp — `tl.so.tilo.discord.preview.CreateWebhookOp`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    p3acbhsm: str  # op
    gu2hv6zx: str  # channel
    ijnwz6yd: str  # tilo


class ChannelSpec(Resource):
    """ChannelSpec — `tl.so.tilo.discord.preview.ChannelSpec`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    aid6dwih: str  # op
    nyneqhtp: str | None  # id
    @property
    def _344fcxyq(self) -> str:
        """tilo (`344fcxyq` on the wire)."""
        return self.fields["344fcxyq"]

    @_344fcxyq.setter
    def _344fcxyq(self, value: str) -> None:
        self.edit("344fcxyq", value=value)
    @property
    def _7qc7rtaa(self) -> ChannelKind:
        """kind (`7qc7rtaa` on the wire)."""
        return self.fields["7qc7rtaa"]

    @_7qc7rtaa.setter
    def _7qc7rtaa(self, value: ChannelKind) -> None:
        self.edit("7qc7rtaa", value=value)
    z5ye67p5: str  # name
    ghxsuztc: str  # topic
    yniihhyc: str | None  # parent
    fvy2cnhd: int  # position


class DeleteChannelOp(Resource):
    """DeleteChannelOp — `tl.so.tilo.discord.preview.DeleteChannelOp`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    rxgm3chv: str  # op
    cvq5uc5z: str  # channel


class CategoryPlace(Resource):
    """CategoryPlace — `tl.so.tilo.discord.preview.CategoryPlace`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    n3kocqem: str  # category
    tjug6eo2: int  # position


class ChannelPlace(Resource):
    """ChannelPlace — `tl.so.tilo.discord.preview.ChannelPlace`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    pposs7uz: str  # channel
    w7v7vvgk: str | None  # parent
    rmnsxntg: int  # position


class ArrangeOp(Resource):
    """ArrangeOp — `tl.so.tilo.discord.preview.ArrangeOp`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    @property
    def _4ebcu3vj(self) -> str:
        """op (`4ebcu3vj` on the wire)."""
        return self.fields["4ebcu3vj"]

    @_4ebcu3vj.setter
    def _4ebcu3vj(self, value: str) -> None:
        self.edit("4ebcu3vj", value=value)
    vamfb4ci: list[CategoryPlace]  # categories
    @property
    def _2jxptmbo(self) -> list[ChannelPlace]:
        """channels (`2jxptmbo` on the wire)."""
        return self.fields["2jxptmbo"]

    @_2jxptmbo.setter
    def _2jxptmbo(self, value: list[ChannelPlace]) -> None:
        self.edit("2jxptmbo", value=value)


class RoleSpec(Resource):
    """RoleSpec — `tl.so.tilo.discord.preview.RoleSpec`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    oad2kmtd: str  # op
    @property
    def _6khaatsw(self) -> str | None:
        """id (`6khaatsw` on the wire)."""
        return self.fields["6khaatsw"]

    @_6khaatsw.setter
    def _6khaatsw(self, value: str | None) -> None:
        self.edit("6khaatsw", value=value)
    @property
    def _6fif6ct6(self) -> str:
        """tilo (`6fif6ct6` on the wire)."""
        return self.fields["6fif6ct6"]

    @_6fif6ct6.setter
    def _6fif6ct6(self, value: str) -> None:
        self.edit("6fif6ct6", value=value)
    x53hs7jf: str  # name
    vmrvsfvk: str  # permissions
    w2v4ktgb: int  # color


class DeleteRoleOp(Resource):
    """DeleteRoleOp — `tl.so.tilo.discord.preview.DeleteRoleOp`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    wxxprqy5: str  # op
    @property
    def _43gurfsq(self) -> str:
        """role (`43gurfsq` on the wire)."""
        return self.fields["43gurfsq"]

    @_43gurfsq.setter
    def _43gurfsq(self, value: str) -> None:
        self.edit("43gurfsq", value=value)


class MemberRolesOp(Resource):
    """MemberRolesOp — `tl.so.tilo.discord.preview.MemberRolesOp`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    kudwdhlw: str  # op
    auupacru: str  # user
    n3pur4nd: list[str]  # add
    xgavnnvp: list[str]  # remove


class MemberOp(Resource):
    """MemberOp — `tl.so.tilo.discord.preview.MemberOp`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    vpb4svn2: str  # op
    w6fejnzs: str  # user
    lwbm7m7g: str  # reason


class OverwriteOp(Resource):
    """OverwriteOp — `tl.so.tilo.discord.preview.OverwriteOp`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    hoy7opkc: str  # op
    @property
    def _5bwuxkgk(self) -> str:
        """channel (`5bwuxkgk` on the wire)."""
        return self.fields["5bwuxkgk"]

    @_5bwuxkgk.setter
    def _5bwuxkgk(self, value: str) -> None:
        self.edit("5bwuxkgk", value=value)
    erieg3ch: str  # target
    cgrybxyv: OverwriteTarget  # target_kind
    wmuwv3mm: str  # allow
    iec2uwfy: str  # deny


class FetchAssetOp(Resource):
    """FetchAssetOp — `tl.so.tilo.discord.preview.FetchAssetOp`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    n5gkbaj7: str  # op
    dymgc2vl: str  # url
    grilu5ek: int  # max_bytes
    @property
    def _4xznvc4c(self) -> bool:
        """inline (`4xznvc4c` on the wire)."""
        return self.fields["4xznvc4c"]

    @_4xznvc4c.setter
    def _4xznvc4c(self, value: bool) -> None:
        self.edit("4xznvc4c", value=value)


class AnswerInteractionOp(Resource):
    """AnswerInteractionOp — `tl.so.tilo.discord.preview.AnswerInteractionOp`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    noppx5yi: str  # op
    ecw6352r: str  # interaction
    ehjwcbzo: str  # content


class EmojiSpec(Resource):
    """EmojiSpec — `tl.so.tilo.discord.preview.EmojiSpec`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    ty6f2nw2: str  # op
    ttzu4a2l: str | None  # id
    bunzoexc: str  # name
    @property
    def _7beod5ga(self) -> str:
        """picture (`7beod5ga` on the wire)."""
        return self.fields["7beod5ga"]

    @_7beod5ga.setter
    def _7beod5ga(self, value: str) -> None:
        self.edit("7beod5ga", value=value)
    vmrzdpp2: str  # tilo


class DeleteEmojiOp(Resource):
    """DeleteEmojiOp — `tl.so.tilo.discord.preview.DeleteEmojiOp`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    w7bai4fx: str  # op
    h2uwhtax: str  # emoji
    @property
    def _3ze5unut(self) -> str:
        """tilo (`3ze5unut` on the wire)."""
        return self.fields["3ze5unut"]

    @_3ze5unut.setter
    def _3ze5unut(self, value: str) -> None:
        self.edit("3ze5unut", value=value)


class Perform(Resource):
    """Perform — `tl.so.tilo.discord.preview.Perform`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    x5ajf4ny: str  # account
    @property
    def _4flmqbgo(self) -> str:
        """op (`4flmqbgo` on the wire)."""
        return self.fields["4flmqbgo"]

    @_4flmqbgo.setter
    def _4flmqbgo(self, value: str) -> None:
        self.edit("4flmqbgo", value=value)


class Accounts(Resource):
    """Accounts — `tl.so.tilo.discord.preview.Accounts`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    z7et5ezu: list[Entry[Discord]]  # accounts

    def begin_setup(self) -> Discord:
        """Starts the account: the record the setup writes into."""
        return self.action("begin_setup")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"begin_setup"})


class Start(Resource):
    """Start — `tl.so.tilo.discord.preview.Start`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    @property
    def _7mzd42ga(self) -> str:
        """url (`7mzd42ga` on the wire)."""
        return self.fields["7mzd42ga"]

    @_7mzd42ga.setter
    def _7mzd42ga(self, value: str) -> None:
        self.edit("7mzd42ga", value=value)
    fpyoqcfm: str  # sha256


class SignInAsk(Resource):
    """SignInAsk — `tl.so.tilo.discord.preview.SignInAsk`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    tqkp4iq7: str  # page


class VoicePair(Resource):
    """VoicePair — `tl.so.tilo.discord.preview.VoicePair`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"
    @property
    def _5xed4gep(self) -> str:
        """discord (`5xed4gep` on the wire)."""
        return self.fields["5xed4gep"]

    @_5xed4gep.setter
    def _5xed4gep(self, value: str) -> None:
        self.edit("5xed4gep", value=value)
    @property
    def _5pvw2ica(self) -> str:
        """tilo (`5pvw2ica` on the wire)."""
        return self.fields["5pvw2ica"]

    @_5pvw2ica.setter
    def _5pvw2ica(self, value: str) -> None:
        self.edit("5pvw2ica", value=value)


class Preview(Resource):
    """Discord — `tl.so.tilo.discord.preview`."""
    __surface__ = "so.tilo.discord.preview@fe34c8ddb5ae477d"

