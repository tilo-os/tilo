"""Types for Discord (`so.tilo.discord`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Guild` is the app's `discord.guild` at `discord/<resource>`, read with
`await ….load()`: its fields are the app's fields (read-only ones without a setter)
and its methods are its actions. A method answers what the app declares; an action
that takes longer answers a `Run`. A record is the dict the wire carries, and an
enum whose cases carry fields is an object whose tag the app's schema names. An
action whose name is one the class already has is run with `action("…")`, which
the file says where it is.
"""

from __future__ import annotations

from typing import Any, Literal, TypedDict
from tilo import Resource, Run

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
    room: str
    host_device: str

class Guild(Resource):
    """Discord server — `discord.guild`."""
    __surface__ = "so.tilo.discord@aeb8e86202ccf6fe"
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
    channels: list[DChannel]
    roles: list[DRole]
    emojis: dict[str, str]
    map: list[Mapping]
    links: list[Link]
    link_offers: list[LinkOffer]
    connects: list[Connect]
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
    ) -> Run | None:
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

    def observe_layout(self,
        *,
        bot_user: str,
        channels: list[ChannelInfo],
        roles: list[RoleInfo],
        emojis: list[EmojiInfo],
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("observe-layout", bot_user=bot_user, channels=channels, roles=roles, emojis=emojis)

    # the app's `map` action is named as a field of this kind: run it with
    # `action("map")`.
    def unmap(self,
        *,
        kind: MappedKind,
        tilo: str,
        keep_apart: bool | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["kind"] = kind
        args["tilo"] = tilo
        if keep_apart is not None:
            args["keep_apart"] = keep_apart
        return self.action("unmap", **args)

    def send(self, *, op: SendOp) -> Any | Run:
        """Runs the app's action."""
        return self.action("send", op=op)

    # the app's `edit` action is the SDK's own: run it with `action("edit")`.
    def delete(self, *, op: DeleteOp) -> Any | Run:
        """Runs the app's action."""
        return self.action("delete", op=op)

    def react(self, *, op: ReactOp) -> Any | Run:
        """Runs the app's action."""
        return self.action("react", op=op)

    def unreact(self, *, op: ReactOp) -> Any | Run:
        """Runs the app's action."""
        return self.action("unreact", op=op)

    def start_thread(self, *, op: StartThreadOp) -> Any | Run:
        """Runs the app's action."""
        return self.action("start-thread", op=op)

    def create_webhook(self, *, op: CreateWebhookOp) -> Any | Run:
        """Runs the app's action."""
        return self.action("create-webhook", op=op)

    def apply_channel(self, *, op: ChannelSpec) -> Any | Run:
        """Runs the app's action."""
        return self.action("apply-channel", op=op)

    def delete_channel(self, *, op: DeleteChannelOp) -> Any | Run:
        """Runs the app's action."""
        return self.action("delete-channel", op=op)

    def arrange_channels(self, *, op: ArrangeOp) -> Any | Run:
        """Runs the app's action."""
        return self.action("arrange-channels", op=op)

    def apply_role(self, *, op: RoleSpec) -> Any | Run:
        """Runs the app's action."""
        return self.action("apply-role", op=op)

    def delete_role(self, *, op: DeleteRoleOp) -> Any | Run:
        """Runs the app's action."""
        return self.action("delete-role", op=op)

    def set_member_roles(self, *, op: MemberRolesOp) -> Any | Run:
        """Runs the app's action."""
        return self.action("set-member-roles", op=op)

    def kick(self, *, op: MemberOp) -> Any | Run:
        """Runs the app's action."""
        return self.action("kick", op=op)

    def ban(self, *, op: MemberOp) -> Any | Run:
        """Runs the app's action."""
        return self.action("ban", op=op)

    def unban(self, *, op: MemberOp) -> Any | Run:
        """Runs the app's action."""
        return self.action("unban", op=op)

    def set_overwrite(self, *, op: OverwriteOp) -> Any | Run:
        """Runs the app's action."""
        return self.action("set-overwrite", op=op)

    def fetch_asset(self, *, op: FetchAssetOp) -> Any | Run:
        """Runs the app's action."""
        return self.action("fetch-asset", op=op)

    def begin_connect(self, *, state_sha256: str) -> Run | None:
        """Runs the app's action."""
        return self.action("begin-connect", state_sha256=state_sha256)

    def offer_link(self,
        *,
        check: str,
        discord_user: str,
        user_name: str,
        display_name: str | None = None,
        avatar: str | None = None,
        interaction: str | None = None,
    ) -> Run | None:
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

    def offer_sign_in(self,
        *,
        check: str,
        discord_user: str,
        user_name: str,
        state_sha256: str,
        display_name: str | None = None,
        avatar: str | None = None,
    ) -> Run | None:
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

    def complete_link(self, *, proof: str) -> Run | None:
        """Runs the app's action."""
        return self.action("complete-link", proof=proof)

    def set_sign_in(self, *, page: str) -> Run | None:
        """Runs the app's action."""
        return self.action("set-sign-in", page=page)

    def answer_interaction(self, *, op: AnswerInteractionOp) -> Any | Run:
        """Runs the app's action."""
        return self.action("answer-interaction", op=op)

    def rename_link(self,
        *,
        discord_user: str,
        user_name: str,
        display_name: str | None = None,
        avatar: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["discord_user"] = discord_user
        args["user_name"] = user_name
        if display_name is not None:
            args["display_name"] = display_name
        if avatar is not None:
            args["avatar"] = avatar
        return self.action("rename-link", **args)

    def set_link_photo(self,
        *,
        discord_user: str,
        avatar: str,
        photo: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-link-photo", discord_user=discord_user, avatar=avatar, photo=photo)

    def unlink(self, *, discord_user: str) -> Run | None:
        """Runs the app's action."""
        return self.action("unlink", discord_user=discord_user)

    def block_persona(self, *, discord_user: str) -> Run | None:
        """Runs the app's action."""
        return self.action("block-persona", discord_user=discord_user)

    def unblock_persona(self, *, discord_user: str) -> Run | None:
        """Runs the app's action."""
        return self.action("unblock-persona", discord_user=discord_user)

    def remember_avatar(self,
        *,
        hash: str,
        blob: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("remember-avatar", hash=hash, blob=blob)

    def finish_import(self, *, tilo_only: list[str]) -> Run | None:
        """Runs the app's action."""
        return self.action("finish-import", tilo_only=tilo_only)

    def apply_emoji(self, *, op: EmojiSpec) -> Any | Run:
        """Runs the app's action."""
        return self.action("apply-emoji", op=op)

    def delete_emoji(self, *, op: DeleteEmojiOp) -> Any | Run:
        """Runs the app's action."""
        return self.action("delete-emoji", op=op)

    def settle_emoji(self,
        *,
        tilo: str,
        result: DiscordResult,
        picture: str | None = None,
        was: str | None = None,
        deleted: bool | None = None,
    ) -> Run | None:
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

    def report_voice(self,
        *,
        problem: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if problem is not None:
            args["problem"] = problem
        return self.action("report-voice", **args)

    def my_link(self) -> None:
        """Runs the app's action."""
        return self.action("my-link")

    def link_offer(self, *, token: Any) -> None:
        """Runs the app's action."""
        return self.action("link-offer", token=token)

    def plan_event(self,
        *,
        event: Any,
        chat: Any,
        context: Context | None = None,
        room: Any | None = None,
        emoji: Any | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["event"] = event
        args["chat"] = chat
        if context is not None:
            args["context"] = context
        if room is not None:
            args["room"] = room
        if emoji is not None:
            args["emoji"] = emoji
        return self.action("plan-event", **args)


