"""Types for Discord (`so.tilo.discord`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.json`;
write it again rather than editing it.

`Guild` is the app's `discord.guild` at `discord/<resource>`, read with
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
    "AnswerInteractionOp",
    "ArrangeOp",
    "CategoryPlace",
    "ChannelInfo",
    "ChannelKind",
    "ChannelPlace",
    "ChannelSpec",
    "Connect",
    "CreateWebhookOp",
    "DChannel",
    "DRole",
    "DeleteChannelOp",
    "DeleteEmojiOp",
    "DeleteOp",
    "DeleteRoleOp",
    "DiscordResult",
    "EditOp",
    "EmojiInfo",
    "EmojiSpec",
    "FetchAssetOp",
    "InAttachment",
    "Link",
    "LinkOffer",
    "MappedKind",
    "Mapping",
    "MemberOp",
    "MemberRolesOp",
    "Origin",
    "OutAttachment",
    "OverwriteInfo",
    "OverwriteKind",
    "OverwriteOp",
    "Ownership",
    "ReactOp",
    "RoleInfo",
    "RoleSpec",
    "SendOp",
    "StartThreadOp",
    "Context",
    "Links",
    "Guild",
    "ConnectArgs",
    "ObserveLayoutArgs",
    "MapArgs",
    "UnmapArgs",
    "BeginConnectArgs",
    "OfferLinkArgs",
    "OfferSignInArgs",
    "CompleteLinkArgs",
    "SetSignInArgs",
    "RenameLinkArgs",
    "SetLinkPhotoArgs",
    "UnlinkArgs",
    "BlockPersonaArgs",
    "UnblockPersonaArgs",
    "RememberAvatarArgs",
    "FinishImportArgs",
    "SettleEmojiArgs",
    "ReportVoiceArgs",
]

class AnswerInteractionOp(TypedDict):
    interaction: str
    content: str

class ArrangeOp(TypedDict):
    categories: list[CategoryPlace]
    channels: list[ChannelPlace]

class CategoryPlace(TypedDict):
    category: str
    position: int

class ChannelInfo(TypedDict):
    id: str
    kind: ChannelKind
    name: str
    parent: str | None
    position: int
    topic: str
    overwrites: list[OverwriteInfo]

ChannelKind = Literal["text", "voice", "category", "thread"]

class ChannelPlace(TypedDict):
    channel: str
    parent: str | None
    position: int

class ChannelSpec(TypedDict):
    id: str | None
    tilo: str
    kind: ChannelKind
    name: str
    topic: str
    parent: str | None
    position: int

class Connect(TypedDict):
    person: str
    state_sha256: str
    expires_ms: int

class CreateWebhookOp(TypedDict):
    channel: str
    tilo: str

class DChannel(TypedDict):
    id: str
    kind: ChannelKind
    name: str
    parent: str | None
    position: int
    topic: str
    overwrites: list[OverwriteInfo]

class DRole(TypedDict):
    id: str
    name: str
    permissions: str
    color: int
    position: int
    managed: bool

class DeleteChannelOp(TypedDict):
    channel: str

class DeleteEmojiOp(TypedDict):
    emoji: str
    tilo: str

class DeleteOp(TypedDict):
    channel: str
    webhook: str | None
    message: str
    thread: str | None

class DeleteRoleOp(TypedDict):
    role: str

DiscordResult = dict[str, Any]
"""One of message, thread, webhook, channel, role, emoji, asset, done: an object whose case the app tags it with."""

class EditOp(TypedDict):
    channel: str
    webhook: str
    message: str
    thread: str | None
    content: str

class EmojiInfo(TypedDict):
    id: str
    name: str
    animated: bool

class EmojiSpec(TypedDict):
    id: str | None
    name: str
    picture: str | None
    tilo: str

class FetchAssetOp(TypedDict):
    url: str
    max_bytes: int
    attachment: InAttachment | None
    inline: bool

InAttachment = dict[str, Any]
"""One of image, video, file: an object whose case the app tags it with."""

class Link(TypedDict):
    discord_user: str
    person: str
    linked_at: int
    user_name: str
    display_name: str | None
    avatar: str | None
    interaction: str | None
    photo: str | None
    photo_avatar: str | None

class LinkOffer(TypedDict):
    check: str
    discord_user: str
    user_name: str
    display_name: str | None
    avatar: str | None
    person: str | None
    interaction: str | None
    expires_ms: int

MappedKind = Literal["channel", "category", "thread", "message", "webhook", "role", "emoji"]

class Mapping(TypedDict):
    key: str
    kind: MappedKind
    tilo: str
    discord: str
    origin: Origin
    persona: str | None
    root: str | None
    picture: str | None

class MemberOp(TypedDict):
    user: str
    reason: str

class MemberRolesOp(TypedDict):
    user: str
    add: list[str]
    remove: list[str]

Origin = Literal["tilo", "discord"]

class OutAttachment(TypedDict):
    blob: str
    name: str
    size: int

class OverwriteInfo(TypedDict):
    id: str
    kind: int
    allow: str
    deny: str

OverwriteKind = Literal["role", "member"]

class OverwriteOp(TypedDict):
    channel: str
    target: str
    target_kind: OverwriteKind
    allow: str
    deny: str

Ownership = Literal["importing", "adopted"]

class ReactOp(TypedDict):
    channel: str
    message: str
    emoji: str

class RoleInfo(TypedDict):
    id: str
    name: str
    permissions: str
    color: int
    position: int
    managed: bool

class RoleSpec(TypedDict):
    id: str | None
    tilo: str
    name: str
    permissions: str
    color: int

class SendOp(TypedDict):
    channel: str
    webhook: str
    thread: str | None
    username: str
    avatar_url: str | None
    avatar_blob: str | None
    content: str
    attachments: list[OutAttachment]
    created_ms: int
    tilo: str

class StartThreadOp(TypedDict):
    channel: str
    message: str
    name: str
    tilo: str

class Context(TypedDict):
    avatars: str
    link: str
    links: Links | None

class Links(TypedDict):
    web_entry: str
    origin: str
    workspace: str
    host_device: str

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
    "channels": "list[ChannelInfo]",
    "roles": "list[RoleInfo]",
    "emojis": "list[EmojiInfo]",
})
"""The arguments of `observe-layout`."""

MapArgs = TypedDict("MapArgs", {
    "kind": "MappedKind",
    "tilo": "str",
    "discord": "str",
    "origin": "Origin",
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
"""The arguments of `begin-connect`."""

OfferLinkArgs = TypedDict("OfferLinkArgs", {
    "check": "str",
    "discord_user": "str",
    "user_name": "str",
    "display_name": NotRequired["str | None"],
    "avatar": NotRequired["str | None"],
    "interaction": NotRequired["str | None"],
})
"""The arguments of `offer-link`."""

OfferSignInArgs = TypedDict("OfferSignInArgs", {
    "check": "str",
    "discord_user": "str",
    "user_name": "str",
    "display_name": NotRequired["str | None"],
    "avatar": NotRequired["str | None"],
    "state_sha256": "str",
})
"""The arguments of `offer-sign-in`."""

CompleteLinkArgs = TypedDict("CompleteLinkArgs", {
    "proof": "str",
})
"""The arguments of `complete-link`."""

SetSignInArgs = TypedDict("SetSignInArgs", {
    "page": "str",
})
"""The arguments of `set-sign-in`."""

RenameLinkArgs = TypedDict("RenameLinkArgs", {
    "discord_user": "str",
    "user_name": "str",
    "display_name": NotRequired["str | None"],
    "avatar": NotRequired["str | None"],
})
"""The arguments of `rename-link`."""

SetLinkPhotoArgs = TypedDict("SetLinkPhotoArgs", {
    "discord_user": "str",
    "avatar": "str",
    "photo": "str",
})
"""The arguments of `set-link-photo`."""

UnlinkArgs = TypedDict("UnlinkArgs", {
    "discord_user": "str",
})
"""The arguments of `unlink`."""

BlockPersonaArgs = TypedDict("BlockPersonaArgs", {
    "discord_user": "str",
})
"""The arguments of `block-persona`."""

UnblockPersonaArgs = TypedDict("UnblockPersonaArgs", {
    "discord_user": "str",
})
"""The arguments of `unblock-persona`."""

RememberAvatarArgs = TypedDict("RememberAvatarArgs", {
    "hash": "str",
    "blob": "str",
})
"""The arguments of `remember-avatar`."""

FinishImportArgs = TypedDict("FinishImportArgs", {
    "tilo_only": "list[str]",
})
"""The arguments of `finish-import`."""

SettleEmojiArgs = TypedDict("SettleEmojiArgs", {
    "tilo": "str",
    "result": "DiscordResult",
    "picture": NotRequired["str | None"],
    "was": NotRequired["str | None"],
    "deleted": NotRequired["bool | None"],
})
"""The arguments of `settle-emoji`."""

ReportVoiceArgs = TypedDict("ReportVoiceArgs", {
    "problem": NotRequired["str | None"],
})
"""The arguments of `report-voice`."""

class Guild(Resource):
    """Discord server — `discord.guild`."""
    __surface__ = "so.tilo.discord@276e12e0544474d6"
    @property
    def guild_id(self) -> str:
        """Read only."""
        return self.fields["guild_id"]
    name: str
    @property
    def only_channel(self) -> str | None:
        """Read only."""
        return self.fields["only_channel"]
    @property
    def mirror_channels(self) -> bool:
        """Read only."""
        return self.fields["mirror_channels"]
    @property
    def map_roles(self) -> bool:
        """Read only."""
        return self.fields["map_roles"]
    ownership: Ownership
    token_item: str | None
    bot_user: str
    channels: list[Entry[DChannel]]
    roles: list[Entry[DRole]]
    emojis: dict[str, str]
    map: list[Entry[Mapping]]
    links: list[Entry[Link]]
    link_offers: list[Entry[LinkOffer]]
    connects: list[Entry[Connect]]
    sign_in_page: str | None
    avatars: dict[str, str]
    blocked_personas: list[str]
    vanished: list[str]
    tilo_only: list[str]
    discord_only: list[str]
    voice_problem: str | None
    emoji_problem: str | None

    def connect(self,
        *,
        guild_id: str,
        name: str,
        only_channel: str | None = None,
        mirror_channels: bool | None = None,
        map_roles: bool | None = None,
    ) -> None:
        """Runs the app's action."""
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
        channels: list[ChannelInfo],
        roles: list[RoleInfo],
        emojis: list[EmojiInfo],
    ) -> None:
        """Runs the app's action."""
        return self.action("observe-layout", bot_user=bot_user, channels=channels, roles=roles, emojis=emojis)

    def observe_layout_many(self, items: list[ObserveLayoutArgs]) -> list[dict[str, Any]]:
        """Runs `observe-layout` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("observe-layout", items)

    # the app's `map` action is named as a field of this kind: run it with
    # `action("map")`.
    def unmap(self,
        *,
        kind: MappedKind,
        tilo: str,
        keep_apart: bool | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["kind"] = kind
        args["tilo"] = tilo
        if keep_apart is not None:
            args["keep_apart"] = keep_apart
        return self.action("unmap", **args)

    def unmap_many(self, items: list[UnmapArgs]) -> list[dict[str, Any]]:
        """Runs `unmap` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("unmap", items)

    def send(self, *, op: SendOp) -> Any:
        """Runs the app's action."""
        return self.action("send", op=op)

    # the app's `edit` action is the SDK's own: run it with `action("edit")`.
    def delete(self, *, op: DeleteOp) -> Any:
        """Runs the app's action."""
        return self.action("delete", op=op)

    def react(self, *, op: ReactOp) -> Any:
        """Runs the app's action."""
        return self.action("react", op=op)

    def unreact(self, *, op: ReactOp) -> Any:
        """Runs the app's action."""
        return self.action("unreact", op=op)

    def start_thread(self, *, op: StartThreadOp) -> Any:
        """Runs the app's action."""
        return self.action("start-thread", op=op)

    def create_webhook(self, *, op: CreateWebhookOp) -> Any:
        """Runs the app's action."""
        return self.action("create-webhook", op=op)

    def apply_channel(self, *, op: ChannelSpec) -> Any:
        """Runs the app's action."""
        return self.action("apply-channel", op=op)

    def delete_channel(self, *, op: DeleteChannelOp) -> Any:
        """Runs the app's action."""
        return self.action("delete-channel", op=op)

    def arrange_channels(self, *, op: ArrangeOp) -> Any:
        """Runs the app's action."""
        return self.action("arrange-channels", op=op)

    def apply_role(self, *, op: RoleSpec) -> Any:
        """Runs the app's action."""
        return self.action("apply-role", op=op)

    def delete_role(self, *, op: DeleteRoleOp) -> Any:
        """Runs the app's action."""
        return self.action("delete-role", op=op)

    def set_member_roles(self, *, op: MemberRolesOp) -> Any:
        """Runs the app's action."""
        return self.action("set-member-roles", op=op)

    def kick(self, *, op: MemberOp) -> Any:
        """Runs the app's action."""
        return self.action("kick", op=op)

    def ban(self, *, op: MemberOp) -> Any:
        """Runs the app's action."""
        return self.action("ban", op=op)

    def unban(self, *, op: MemberOp) -> Any:
        """Runs the app's action."""
        return self.action("unban", op=op)

    def set_overwrite(self, *, op: OverwriteOp) -> Any:
        """Runs the app's action."""
        return self.action("set-overwrite", op=op)

    def fetch_asset(self, *, op: FetchAssetOp) -> Any:
        """Runs the app's action."""
        return self.action("fetch-asset", op=op)

    def begin_connect(self, *, state_sha256: str) -> None:
        """Runs the app's action."""
        return self.action("begin-connect", state_sha256=state_sha256)

    def begin_connect_many(self, items: list[BeginConnectArgs]) -> list[dict[str, Any]]:
        """Runs `begin-connect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("begin-connect", items)

    def offer_link(self,
        *,
        check: str,
        discord_user: str,
        user_name: str,
        display_name: str | None = None,
        avatar: str | None = None,
        interaction: str | None = None,
    ) -> None:
        """Runs the app's action."""
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
        return self.action("offer-link", **args)

    def offer_link_many(self, items: list[OfferLinkArgs]) -> list[dict[str, Any]]:
        """Runs `offer-link` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("offer-link", items)

    def offer_sign_in(self,
        *,
        check: str,
        discord_user: str,
        user_name: str,
        state_sha256: str,
        display_name: str | None = None,
        avatar: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["check"] = check
        args["discord_user"] = discord_user
        args["user_name"] = user_name
        if display_name is not None:
            args["display_name"] = display_name
        if avatar is not None:
            args["avatar"] = avatar
        args["state_sha256"] = state_sha256
        return self.action("offer-sign-in", **args)

    def offer_sign_in_many(self, items: list[OfferSignInArgs]) -> list[dict[str, Any]]:
        """Runs `offer-sign-in` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("offer-sign-in", items)

    def complete_link(self, *, proof: str) -> None:
        """Runs the app's action."""
        return self.action("complete-link", proof=proof)

    def complete_link_many(self, items: list[CompleteLinkArgs]) -> list[dict[str, Any]]:
        """Runs `complete-link` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("complete-link", items)

    def set_sign_in(self, *, page: str) -> None:
        """Runs the app's action."""
        return self.action("set-sign-in", page=page)

    def set_sign_in_many(self, items: list[SetSignInArgs]) -> list[dict[str, Any]]:
        """Runs `set-sign-in` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-sign-in", items)

    def answer_interaction(self, *, op: AnswerInteractionOp) -> Any:
        """Runs the app's action."""
        return self.action("answer-interaction", op=op)

    def rename_link(self,
        *,
        discord_user: str,
        user_name: str,
        display_name: str | None = None,
        avatar: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["discord_user"] = discord_user
        args["user_name"] = user_name
        if display_name is not None:
            args["display_name"] = display_name
        if avatar is not None:
            args["avatar"] = avatar
        return self.action("rename-link", **args)

    def rename_link_many(self, items: list[RenameLinkArgs]) -> list[dict[str, Any]]:
        """Runs `rename-link` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rename-link", items)

    def set_link_photo(self,
        *,
        discord_user: str,
        avatar: str,
        photo: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-link-photo", discord_user=discord_user, avatar=avatar, photo=photo)

    def set_link_photo_many(self, items: list[SetLinkPhotoArgs]) -> list[dict[str, Any]]:
        """Runs `set-link-photo` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-link-photo", items)

    def unlink(self, *, discord_user: str) -> None:
        """Runs the app's action."""
        return self.action("unlink", discord_user=discord_user)

    def unlink_many(self, items: list[UnlinkArgs]) -> list[dict[str, Any]]:
        """Runs `unlink` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("unlink", items)

    def block_persona(self, *, discord_user: str) -> None:
        """Runs the app's action."""
        return self.action("block-persona", discord_user=discord_user)

    def block_persona_many(self, items: list[BlockPersonaArgs]) -> list[dict[str, Any]]:
        """Runs `block-persona` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("block-persona", items)

    def unblock_persona(self, *, discord_user: str) -> None:
        """Runs the app's action."""
        return self.action("unblock-persona", discord_user=discord_user)

    def unblock_persona_many(self, items: list[UnblockPersonaArgs]) -> list[dict[str, Any]]:
        """Runs `unblock-persona` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("unblock-persona", items)

    def remember_avatar(self,
        *,
        hash: str,
        blob: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("remember-avatar", hash=hash, blob=blob)

    def remember_avatar_many(self, items: list[RememberAvatarArgs]) -> list[dict[str, Any]]:
        """Runs `remember-avatar` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remember-avatar", items)

    def finish_import(self, *, tilo_only: list[str]) -> None:
        """Runs the app's action."""
        return self.action("finish-import", tilo_only=tilo_only)

    def finish_import_many(self, items: list[FinishImportArgs]) -> list[dict[str, Any]]:
        """Runs `finish-import` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("finish-import", items)

    def apply_emoji(self, *, op: EmojiSpec) -> Any:
        """Runs the app's action."""
        return self.action("apply-emoji", op=op)

    def delete_emoji(self, *, op: DeleteEmojiOp) -> Any:
        """Runs the app's action."""
        return self.action("delete-emoji", op=op)

    def settle_emoji(self,
        *,
        tilo: str,
        result: DiscordResult,
        picture: str | None = None,
        was: str | None = None,
        deleted: bool | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["tilo"] = tilo
        args["result"] = result
        if picture is not None:
            args["picture"] = picture
        if was is not None:
            args["was"] = was
        if deleted is not None:
            args["deleted"] = deleted
        return self.action("settle-emoji", **args)

    def settle_emoji_many(self, items: list[SettleEmojiArgs]) -> list[dict[str, Any]]:
        """Runs `settle-emoji` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("settle-emoji", items)

    def report_voice(self,
        *,
        problem: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if problem is not None:
            args["problem"] = problem
        return self.action("report-voice", **args)

    def report_voice_many(self, items: list[ReportVoiceArgs]) -> list[dict[str, Any]]:
        """Runs `report-voice` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("report-voice", items)

    def my_link(self) -> None:
        """Runs the app's action."""
        return self.action("my-link")

    def link_offer(self, *, token: str) -> None:
        """Runs the app's action."""
        return self.action("link-offer", token=token)

    def plan_event(self,
        *,
        event: Any,
        chat: Any,
        context: Context | None = None,
        workspace: Any | None = None,
        emoji: Any | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["event"] = event
        args["chat"] = chat
        if context is not None:
            args["context"] = context
        if workspace is not None:
            args["workspace"] = workspace
        if emoji is not None:
            args["emoji"] = emoji
        return self.action("plan-event", **args)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"connect", "connect_many", "observe_layout", "observe_layout_many", "unmap", "unmap_many", "send", "delete", "react", "unreact", "start_thread", "create_webhook", "apply_channel", "delete_channel", "arrange_channels", "apply_role", "delete_role", "set_member_roles", "kick", "ban", "unban", "set_overwrite", "fetch_asset", "begin_connect", "begin_connect_many", "offer_link", "offer_link_many", "offer_sign_in", "offer_sign_in_many", "complete_link", "complete_link_many", "set_sign_in", "set_sign_in_many", "answer_interaction", "rename_link", "rename_link_many", "set_link_photo", "set_link_photo_many", "unlink", "unlink_many", "block_persona", "block_persona_many", "unblock_persona", "unblock_persona_many", "remember_avatar", "remember_avatar_many", "finish_import", "finish_import_many", "apply_emoji", "delete_emoji", "settle_emoji", "settle_emoji_many", "report_voice", "report_voice_many", "my_link", "link_offer", "plan_event"})


