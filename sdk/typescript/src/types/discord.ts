/**
 * Types for Discord (`so.tilo.discord`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Guild` is a `Resource` of the app's `discord.guild`, made by its factory with the
 * resource's path (`discord/<resource>`): its attributes are the app's fields
 * (read-only ones `readonly`) and its methods are the app's actions, named in camel
 * case and taking the arguments the wire takes them under. An action that takes
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "@tilo/sdk";
import type * as tilo from "@tilo/sdk";

export interface AnswerInteractionOp {
  interaction: string;
  content: string;
}

export interface ArrangeOp {
  categories: CategoryPlace[];
  channels: ChannelPlace[];
}

export interface CategoryPlace {
  category: string;
  position: number;
}

export interface ChannelInfo {
  id: string;
  kind: ChannelKind;
  name: string;
  parent: string | null;
  position: number;
  topic: string;
  overwrites: OverwriteInfo[];
}

/** One of "text", "voice", "category", "thread". */
export type ChannelKind = "text" | "voice" | "category" | "thread";

export interface ChannelPlace {
  channel: string;
  parent: string | null;
  position: number;
}

export interface ChannelSpec {
  id: string | null;
  tilo: string;
  kind: ChannelKind;
  name: string;
  topic: string;
  parent: string | null;
  position: number;
}

export interface Connect {
  person: string;
  state_sha256: string;
  expires_ms: number;
}

export interface CreateWebhookOp {
  channel: string;
  tilo: string;
}

export interface DChannel {
  id: string;
  kind: ChannelKind;
  name: string;
  parent: string | null;
  position: number;
  topic: string;
  overwrites: OverwriteInfo[];
}

export interface DRole {
  id: string;
  name: string;
  permissions: string;
  color: number;
  position: number;
  managed: boolean;
}

export interface DeleteChannelOp {
  channel: string;
}

export interface DeleteEmojiOp {
  emoji: string;
  tilo: string;
}

export interface DeleteOp {
  channel: string;
  webhook: string | null;
  message: string;
  thread: string | null;
}

export interface DeleteRoleOp {
  role: string;
}

/** One of message, thread, webhook, channel, role, emoji, asset, done: an object whose case the app tags it with. */
export type DiscordResult = Record<string, unknown>;

export interface EditOp {
  channel: string;
  webhook: string;
  message: string;
  thread: string | null;
  content: string;
}

export interface EmojiInfo {
  id: string;
  name: string;
  animated: boolean;
}

export interface EmojiSpec {
  id: string | null;
  name: string;
  picture: string | null;
  tilo: string;
}

export interface FetchAssetOp {
  url: string;
  max_bytes: number;
  attachment: InAttachment | null;
  inline: boolean;
}

/** One of image, video, file: an object whose case the app tags it with. */
export type InAttachment = Record<string, unknown>;

export interface Link {
  discord_user: string;
  person: string;
  linked_at: number;
  user_name: string;
  display_name: string | null;
  avatar: string | null;
  interaction: string | null;
  photo: string | null;
  photo_avatar: string | null;
}

export interface LinkOffer {
  check: string;
  discord_user: string;
  user_name: string;
  display_name: string | null;
  avatar: string | null;
  person: string | null;
  interaction: string | null;
  expires_ms: number;
}

/** One of "channel", "category", "thread", "message", "webhook", "role", "emoji". */
export type MappedKind = "channel" | "category" | "thread" | "message" | "webhook" | "role" | "emoji";

export interface Mapping {
  key: string;
  kind: MappedKind;
  tilo: string;
  discord: string;
  origin: Origin;
  persona: string | null;
  root: string | null;
  picture: string | null;
}

export interface MemberOp {
  user: string;
  reason: string;
}

export interface MemberRolesOp {
  user: string;
  add: string[];
  remove: string[];
}

/** One of "tilo", "discord". */
export type Origin = "tilo" | "discord";

export interface OutAttachment {
  blob: string;
  name: string;
  size: number;
}

export interface OverwriteInfo {
  id: string;
  kind: number;
  allow: string;
  deny: string;
}

/** One of "role", "member". */
export type OverwriteKind = "role" | "member";

export interface OverwriteOp {
  channel: string;
  target: string;
  target_kind: OverwriteKind;
  allow: string;
  deny: string;
}

/** One of "importing", "adopted". */
export type Ownership = "importing" | "adopted";

export interface ReactOp {
  channel: string;
  message: string;
  emoji: string;
}

export interface RoleInfo {
  id: string;
  name: string;
  permissions: string;
  color: number;
  position: number;
  managed: boolean;
}

export interface RoleSpec {
  id: string | null;
  tilo: string;
  name: string;
  permissions: string;
  color: number;
}

export interface SendOp {
  channel: string;
  webhook: string;
  thread: string | null;
  username: string;
  avatar_url: string | null;
  avatar_blob: string | null;
  content: string;
  attachments: OutAttachment[];
  created_ms: number;
  tilo: string;
}

export interface StartThreadOp {
  channel: string;
  message: string;
  name: string;
  tilo: string;
}

export interface Context {
  avatars: string;
  link: string;
  links: Links | null;
}

export interface Links {
  web_entry: string;
  origin: string;
  room: string;
  host_device: string;
}

/** Discord server — `discord.guild`. */
export interface Guild extends tilo.Resource {
  readonly guild_id: string;
  name: string;
  readonly only_channel: string | null;
  readonly mirror_channels: boolean;
  readonly map_roles: boolean;
  ownership: Ownership;
  token_item: string | null;
  bot_user: string;
  channels: DChannel[];
  roles: DRole[];
  emojis: Record<string, string>;
  map: Mapping[];
  links: Link[];
  link_offers: LinkOffer[];
  connects: Connect[];
  sign_in_page: string | null;
  avatars: Record<string, string>;
  blocked_personas: string[];
  vanished: string[];
  tilo_only: string[];
  discord_only: string[];
  voice_problem: string | null;
  emoji_problem: string | null;
  connect(args: {
    guild_id: string;
    name: string;
    only_channel?: string;
    mirror_channels?: boolean;
    map_roles?: boolean;
  }): Promise<tilo.Run | void>;
  observeLayout(args: {
    bot_user: string;
    channels: ChannelInfo[];
    roles: RoleInfo[];
    emojis: EmojiInfo[];
  }): Promise<tilo.Run | void>;
  // the app's `map` action is named as a field of this kind: run it with
  // `action("map")`.
  unmap(args: {
    kind: MappedKind;
    tilo: string;
    keep_apart?: boolean;
  }): Promise<tilo.Run | void>;
  send(args: {
    op: SendOp;
  }): Promise<unknown | tilo.Run>;
  // the app's `edit` action is the SDK's own: run it with `action("edit")`.
  delete(args: {
    op: DeleteOp;
  }): Promise<unknown | tilo.Run>;
  react(args: {
    op: ReactOp;
  }): Promise<unknown | tilo.Run>;
  unreact(args: {
    op: ReactOp;
  }): Promise<unknown | tilo.Run>;
  startThread(args: {
    op: StartThreadOp;
  }): Promise<unknown | tilo.Run>;
  createWebhook(args: {
    op: CreateWebhookOp;
  }): Promise<unknown | tilo.Run>;
  applyChannel(args: {
    op: ChannelSpec;
  }): Promise<unknown | tilo.Run>;
  deleteChannel(args: {
    op: DeleteChannelOp;
  }): Promise<unknown | tilo.Run>;
  arrangeChannels(args: {
    op: ArrangeOp;
  }): Promise<unknown | tilo.Run>;
  applyRole(args: {
    op: RoleSpec;
  }): Promise<unknown | tilo.Run>;
  deleteRole(args: {
    op: DeleteRoleOp;
  }): Promise<unknown | tilo.Run>;
  setMemberRoles(args: {
    op: MemberRolesOp;
  }): Promise<unknown | tilo.Run>;
  kick(args: {
    op: MemberOp;
  }): Promise<unknown | tilo.Run>;
  ban(args: {
    op: MemberOp;
  }): Promise<unknown | tilo.Run>;
  unban(args: {
    op: MemberOp;
  }): Promise<unknown | tilo.Run>;
  setOverwrite(args: {
    op: OverwriteOp;
  }): Promise<unknown | tilo.Run>;
  fetchAsset(args: {
    op: FetchAssetOp;
  }): Promise<unknown | tilo.Run>;
  beginConnect(args: {
    state_sha256: string;
  }): Promise<tilo.Run | void>;
  offerLink(args: {
    check: string;
    discord_user: string;
    user_name: string;
    display_name?: string;
    avatar?: string;
    interaction?: string;
  }): Promise<tilo.Run | void>;
  offerSignIn(args: {
    check: string;
    discord_user: string;
    user_name: string;
    state_sha256: string;
    display_name?: string;
    avatar?: string;
  }): Promise<tilo.Run | void>;
  completeLink(args: {
    proof: string;
  }): Promise<tilo.Run | void>;
  setSignIn(args: {
    page: string;
  }): Promise<tilo.Run | void>;
  answerInteraction(args: {
    op: AnswerInteractionOp;
  }): Promise<unknown | tilo.Run>;
  renameLink(args: {
    discord_user: string;
    user_name: string;
    display_name?: string;
    avatar?: string;
  }): Promise<tilo.Run | void>;
  setLinkPhoto(args: {
    discord_user: string;
    avatar: string;
    photo: string;
  }): Promise<tilo.Run | void>;
  unlink(args: {
    discord_user: string;
  }): Promise<tilo.Run | void>;
  blockPersona(args: {
    discord_user: string;
  }): Promise<tilo.Run | void>;
  unblockPersona(args: {
    discord_user: string;
  }): Promise<tilo.Run | void>;
  rememberAvatar(args: {
    hash: string;
    blob: string;
  }): Promise<tilo.Run | void>;
  finishImport(args: {
    tilo_only: string[];
  }): Promise<tilo.Run | void>;
  applyEmoji(args: {
    op: EmojiSpec;
  }): Promise<unknown | tilo.Run>;
  deleteEmoji(args: {
    op: DeleteEmojiOp;
  }): Promise<unknown | tilo.Run>;
  settleEmoji(args: {
    tilo: string;
    result: DiscordResult;
    picture?: string;
    was?: string;
    deleted?: boolean;
  }): Promise<tilo.Run | void>;
  reportVoice(args?: {
    problem?: string;
  }): Promise<tilo.Run | void>;
  myLink(): Promise<void>;
  linkOffer(args: {
    token: unknown;
  }): Promise<void>;
  planRoute(args: {
    event: unknown;
  }): Promise<void>;
  planEvent(args: {
    event: unknown;
    chat: unknown;
    context?: Context;
    room?: unknown;
    emoji?: unknown;
  }): Promise<void>;
}

/** The `discord.guild` kind at `path`. */
export function guild(ws: tilo.Workspace, path: string): Guild {
  return resourceProxy(ws, path) as unknown as Guild;
}

