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
 * longer answers an `Invocation`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is. An undoable action that answers nothing also has a `<action>Many`,
which applies it to every argument object of a list in a few all-or-nothing steps,
all of them one invocation group.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** The version of the surface these types were made from: a workspace whose app
 * publishes another refuses their actions rather than run what they no longer
 * describe. */
export const SURFACE = "so.tilo.discord@8cdd5fc7bfe08e63";

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
  workspace: string;
  host_device: string;
}

/** The arguments of `connect`. */
export interface ConnectArgs {
  guild_id: string;
  name: string;
  only_channel?: string;
  mirror_channels?: boolean;
  map_roles?: boolean;
}

/** The arguments of `observe-layout`. */
export interface ObserveLayoutArgs {
  bot_user: string;
  channels: ChannelInfo[];
  roles: RoleInfo[];
  emojis: EmojiInfo[];
}

/** The arguments of `map`. */
export interface MapArgs {
  kind: MappedKind;
  tilo: string;
  discord: string;
  origin: Origin;
  persona?: string;
  root?: string;
}

/** The arguments of `unmap`. */
export interface UnmapArgs {
  kind: MappedKind;
  tilo: string;
  keep_apart?: boolean;
}

/** The arguments of `begin-connect`. */
export interface BeginConnectArgs {
  state_sha256: string;
}

/** The arguments of `offer-link`. */
export interface OfferLinkArgs {
  check: string;
  discord_user: string;
  user_name: string;
  display_name?: string;
  avatar?: string;
  interaction?: string;
}

/** The arguments of `offer-sign-in`. */
export interface OfferSignInArgs {
  check: string;
  discord_user: string;
  user_name: string;
  state_sha256: string;
  display_name?: string;
  avatar?: string;
}

/** The arguments of `complete-link`. */
export interface CompleteLinkArgs {
  proof: string;
}

/** The arguments of `set-sign-in`. */
export interface SetSignInArgs {
  page: string;
}

/** The arguments of `rename-link`. */
export interface RenameLinkArgs {
  discord_user: string;
  user_name: string;
  display_name?: string;
  avatar?: string;
}

/** The arguments of `set-link-photo`. */
export interface SetLinkPhotoArgs {
  discord_user: string;
  avatar: string;
  photo: string;
}

/** The arguments of `unlink`. */
export interface UnlinkArgs {
  discord_user: string;
}

/** The arguments of `block-persona`. */
export interface BlockPersonaArgs {
  discord_user: string;
}

/** The arguments of `unblock-persona`. */
export interface UnblockPersonaArgs {
  discord_user: string;
}

/** The arguments of `remember-avatar`. */
export interface RememberAvatarArgs {
  hash: string;
  blob: string;
}

/** The arguments of `finish-import`. */
export interface FinishImportArgs {
  tilo_only: string[];
}

/** The arguments of `settle-emoji`. */
export interface SettleEmojiArgs {
  tilo: string;
  result: DiscordResult;
  picture?: string;
  was?: string;
  deleted?: boolean;
}

/** The arguments of `report-voice`. */
export interface ReportVoiceArgs {
  problem?: string;
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
  channels: Array<tilo.Entry<DChannel>>;
  roles: Array<tilo.Entry<DRole>>;
  emojis: Record<string, string>;
  map: Array<tilo.Entry<Mapping>>;
  links: Array<tilo.Entry<Link>>;
  link_offers: Array<tilo.Entry<LinkOffer>>;
  connects: Array<tilo.Entry<Connect>>;
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
  }): Promise<void>;
  /** Runs `connect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  connectMany(items: ConnectArgs[]): Promise<tilo.ActionAnswer[]>;
  observeLayout(args: {
    bot_user: string;
    channels: ChannelInfo[];
    roles: RoleInfo[];
    emojis: EmojiInfo[];
  }): Promise<void>;
  /** Runs `observe-layout` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  observeLayoutMany(items: ObserveLayoutArgs[]): Promise<tilo.ActionAnswer[]>;
  // the app's `map` action is named as a field of this kind: run it with
  // `action("map")`.
  unmap(args: {
    kind: MappedKind;
    tilo: string;
    keep_apart?: boolean;
  }): Promise<void>;
  /** Runs `unmap` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  unmapMany(items: UnmapArgs[]): Promise<tilo.ActionAnswer[]>;
  send(args: {
    op: SendOp;
  }): Promise<unknown>;
  // the app's `edit` action is the SDK's own: run it with `action("edit")`.
  delete(args: {
    op: DeleteOp;
  }): Promise<unknown>;
  react(args: {
    op: ReactOp;
  }): Promise<unknown>;
  unreact(args: {
    op: ReactOp;
  }): Promise<unknown>;
  startThread(args: {
    op: StartThreadOp;
  }): Promise<unknown>;
  createWebhook(args: {
    op: CreateWebhookOp;
  }): Promise<unknown>;
  applyChannel(args: {
    op: ChannelSpec;
  }): Promise<unknown>;
  deleteChannel(args: {
    op: DeleteChannelOp;
  }): Promise<unknown>;
  arrangeChannels(args: {
    op: ArrangeOp;
  }): Promise<unknown>;
  applyRole(args: {
    op: RoleSpec;
  }): Promise<unknown>;
  deleteRole(args: {
    op: DeleteRoleOp;
  }): Promise<unknown>;
  setMemberRoles(args: {
    op: MemberRolesOp;
  }): Promise<unknown>;
  kick(args: {
    op: MemberOp;
  }): Promise<unknown>;
  ban(args: {
    op: MemberOp;
  }): Promise<unknown>;
  unban(args: {
    op: MemberOp;
  }): Promise<unknown>;
  setOverwrite(args: {
    op: OverwriteOp;
  }): Promise<unknown>;
  fetchAsset(args: {
    op: FetchAssetOp;
  }): Promise<unknown>;
  beginConnect(args: {
    state_sha256: string;
  }): Promise<void>;
  /** Runs `begin-connect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  beginConnectMany(items: BeginConnectArgs[]): Promise<tilo.ActionAnswer[]>;
  offerLink(args: {
    check: string;
    discord_user: string;
    user_name: string;
    display_name?: string;
    avatar?: string;
    interaction?: string;
  }): Promise<void>;
  /** Runs `offer-link` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  offerLinkMany(items: OfferLinkArgs[]): Promise<tilo.ActionAnswer[]>;
  offerSignIn(args: {
    check: string;
    discord_user: string;
    user_name: string;
    state_sha256: string;
    display_name?: string;
    avatar?: string;
  }): Promise<void>;
  /** Runs `offer-sign-in` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  offerSignInMany(items: OfferSignInArgs[]): Promise<tilo.ActionAnswer[]>;
  completeLink(args: {
    proof: string;
  }): Promise<void>;
  /** Runs `complete-link` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  completeLinkMany(items: CompleteLinkArgs[]): Promise<tilo.ActionAnswer[]>;
  setSignIn(args: {
    page: string;
  }): Promise<void>;
  /** Runs `set-sign-in` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setSignInMany(items: SetSignInArgs[]): Promise<tilo.ActionAnswer[]>;
  answerInteraction(args: {
    op: AnswerInteractionOp;
  }): Promise<unknown>;
  renameLink(args: {
    discord_user: string;
    user_name: string;
    display_name?: string;
    avatar?: string;
  }): Promise<void>;
  /** Runs `rename-link` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  renameLinkMany(items: RenameLinkArgs[]): Promise<tilo.ActionAnswer[]>;
  setLinkPhoto(args: {
    discord_user: string;
    avatar: string;
    photo: string;
  }): Promise<void>;
  /** Runs `set-link-photo` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setLinkPhotoMany(items: SetLinkPhotoArgs[]): Promise<tilo.ActionAnswer[]>;
  unlink(args: {
    discord_user: string;
  }): Promise<void>;
  /** Runs `unlink` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  unlinkMany(items: UnlinkArgs[]): Promise<tilo.ActionAnswer[]>;
  blockPersona(args: {
    discord_user: string;
  }): Promise<void>;
  /** Runs `block-persona` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  blockPersonaMany(items: BlockPersonaArgs[]): Promise<tilo.ActionAnswer[]>;
  unblockPersona(args: {
    discord_user: string;
  }): Promise<void>;
  /** Runs `unblock-persona` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  unblockPersonaMany(items: UnblockPersonaArgs[]): Promise<tilo.ActionAnswer[]>;
  rememberAvatar(args: {
    hash: string;
    blob: string;
  }): Promise<void>;
  /** Runs `remember-avatar` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rememberAvatarMany(items: RememberAvatarArgs[]): Promise<tilo.ActionAnswer[]>;
  finishImport(args: {
    tilo_only: string[];
  }): Promise<void>;
  /** Runs `finish-import` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  finishImportMany(items: FinishImportArgs[]): Promise<tilo.ActionAnswer[]>;
  applyEmoji(args: {
    op: EmojiSpec;
  }): Promise<unknown>;
  deleteEmoji(args: {
    op: DeleteEmojiOp;
  }): Promise<unknown>;
  settleEmoji(args: {
    tilo: string;
    result: DiscordResult;
    picture?: string;
    was?: string;
    deleted?: boolean;
  }): Promise<void>;
  /** Runs `settle-emoji` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  settleEmojiMany(items: SettleEmojiArgs[]): Promise<tilo.ActionAnswer[]>;
  reportVoice(args?: {
    problem?: string;
  }): Promise<void>;
  /** Runs `report-voice` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  reportVoiceMany(items: ReportVoiceArgs[]): Promise<tilo.ActionAnswer[]>;
  myLink(): Promise<void>;
  linkOffer(args: {
    token: string;
  }): Promise<void>;
  planEvent(args: {
    event: unknown;
    chat: unknown;
    context?: Context;
    workspace?: unknown;
    emoji?: unknown;
  }): Promise<void>;
}

/** The actions `Guild` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const GUILD_ACTIONS = ["connect", "connectMany", "observeLayout", "observeLayoutMany", "unmap", "unmapMany", "send", "delete", "react", "unreact", "startThread", "createWebhook", "applyChannel", "deleteChannel", "arrangeChannels", "applyRole", "deleteRole", "setMemberRoles", "kick", "ban", "unban", "setOverwrite", "fetchAsset", "beginConnect", "beginConnectMany", "offerLink", "offerLinkMany", "offerSignIn", "offerSignInMany", "completeLink", "completeLinkMany", "setSignIn", "setSignInMany", "answerInteraction", "renameLink", "renameLinkMany", "setLinkPhoto", "setLinkPhotoMany", "unlink", "unlinkMany", "blockPersona", "blockPersonaMany", "unblockPersona", "unblockPersonaMany", "rememberAvatar", "rememberAvatarMany", "finishImport", "finishImportMany", "applyEmoji", "deleteEmoji", "settleEmoji", "settleEmojiMany", "reportVoice", "reportVoiceMany", "myLink", "linkOffer", "planEvent"] as const;

/** The `discord.guild` kind at `path`. */
export function guild(ws: tilo.Workspace, path: string): Guild {
  return resourceProxy(ws, path, SURFACE, GUILD_ACTIONS) as unknown as Guild;
}

