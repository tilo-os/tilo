/**
 * Types for Discord (`so.tilo.discord.preview`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`preview/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.discord.preview@b341ab6e1fe55808";

/** What kind of place a Discord channel is. One of "text", "voice", "category", "thread". */
export type ChannelKind = "text" | "voice" | "category" | "thread";

/** What a mapping joins. One of "channel", "category", "thread", "message", "webhook", "role", "emoji". */
export type MappedKind = "channel" | "category" | "thread" | "message" | "webhook" | "role" | "emoji";

/** Which side a mapped object was made on. One of "tilo", "discord". */
export type OriginSide = "tilo" | "discord";

/** Whether the server is still being brought in, or bridged both ways. One of "importing", "adopted". */
export type Ownership = "importing" | "adopted";

/** Whose permission overwrite an op sets. One of "role", "member". */
export type OverwriteTarget = "role" | "member";

/** The arguments of `found_channels`. */
export interface FoundChannelsArgs {
  ask: string;
  channels: DChannel[];
}

/** The arguments of `choose_channel`. */
export interface ChooseChannelArgs {
  channel: string;
}

/** The arguments of `back_to_servers`. */
export interface BackToServersArgs {
}

/** The arguments of `map`. */
export interface MapArgs {
  kind: MappedKind;
  tilo: string;
  discord: string;
  origin?: OriginSide;
  persona?: string;
  root?: string;
}

/** The arguments of `unmap`. */
export interface UnmapArgs {
  kind: MappedKind;
  tilo: string;
  keep_apart?: boolean;
}

/** The arguments of `offer_link`. */
export interface OfferLinkArgs {
  check: string;
  discord_user: string;
  user_name: string;
  display_name?: string;
  avatar?: string;
  interaction?: string;
}

/** The arguments of `offer_sign_in`. */
export interface OfferSignInArgs {
  check: string;
  discord_user: string;
  user_name: string;
  state_sha256: string;
  display_name?: string;
  avatar?: string;
}

/** The arguments of `complete_link`. */
export interface CompleteLinkArgs {
  proof: string;
}

/** The arguments of `rename_link`. */
export interface RenameLinkArgs {
  discord_user: string;
  user_name: string;
  display_name?: string;
  avatar?: string;
}

/** The arguments of `set_link_photo`. */
export interface SetLinkPhotoArgs {
  discord_user: string;
  avatar: string;
  photo: string;
}

/** The arguments of `unlink`. */
export interface UnlinkArgs {
  discord_user: string;
}

/** The arguments of `unlink_mine`. */
export interface UnlinkMineArgs {
}

/** The arguments of `block_persona`. */
export interface BlockPersonaArgs {
  discord_user: string;
}

/** The arguments of `unblock_persona`. */
export interface UnblockPersonaArgs {
  discord_user: string;
}

/** The arguments of `remember_avatar`. */
export interface RememberAvatarArgs {
  hash: string;
  blob: string;
}

/** The arguments of `finish_import`. */
export interface FinishImportArgs {
  tilo_only: string[];
}

/** Notice — `tl.tilo.host.Notice`. */
export interface Notice extends tilo.Resource {
  notice_to: string;  // to
  notice_title: string;  // title
  notice_body: string;  // body
  notice_opens: unknown;  // opens
  notice_seen: boolean;  // seen
  notice_device: string;  // device
  notice_actions: string[];  // actions
}

/** The actions `Notice` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const NOTICE_ACTIONS = [] as const;

/** The `tl.tilo.host.Notice` kind at `path`. */
export function notice(ws: tilo.Workspace, path: string): Notice {
  return resourceProxy(ws, path, SURFACE, NOTICE_ACTIONS) as unknown as Notice;
}

/** Discord — `tl.so.tilo.discord.preview.Discord`. */
export interface Discord extends tilo.Resource {
  sclfytrh: string;  // name
  lex22ami: string;  // guild_id
  "2mt4scyz": string;  // server
  zoqgfxmk: string;  // only_channel
  sqlamhjd: boolean;  // mirror_channels
  bxzjsd2v: boolean;  // map_roles
  jf2mmurb: Ownership;  // ownership
  "6hprgb4l": string;  // token_item
  lp5din5r: string;  // api_origin
  ylfbjmz6: string;  // gateway_url
  k3mrdk3z: string;  // bot_user
  pgejogzq: Array<tilo.Entry<DChannel>>;  // channels
  zhmdub7y: Array<tilo.Entry<DRole>>;  // roles
  aphz34n7: Array<tilo.Entry<DEmoji>>;  // emojis
  ma5o2ina: Array<tilo.Entry<Mapping>>;  // map
  ec3vplzb: Array<tilo.Entry<Link>>;  // links
  pf7f3cf2: Array<tilo.Entry<LinkOffer>>;  // link_offers
  kaj3zmhy: Array<tilo.Entry<Connect>>;  // connects
  db4wn64y: string;  // sign_in_page
  bkxzddk4: string;  // sign_in_url
  bdu3hrmn: Record<string, string>;  // avatars
  "6g22oebp": string[];  // blocked_personas
  y6utf74j: string[];  // vanished
  htrhhc47: string[];  // tilo_only
  b7dtt5ts: string[];  // discord_only
  nn6mtwfm: string;  // voice_problem
  flkdculv: string;  // emoji_problem
  "2vnxydnl": boolean;  // setup_open
  xezmxd63: string;  // setup_wants
  idxrloj3: string;  // setup_ask
  alwcte5y: string;  // setup_person
  "6fcfq4y6": Array<tilo.Entry<ServerChoice>>;  // setup_servers
  gsce46aw: string;  // setup_server
  ewopezjx: string;  // setup_server_name
  vwt52f57: Array<tilo.Entry<DChannel>>;  // setup_channels
  wuvfepf6: string;  // setup_channel
  xeugfqli: boolean;  // setup_mirror
  w2b5pndu: boolean;  // setup_roles
  pasjy4ku: string;  // setup_problem
  open_setup(): Promise<void>;
  /** Closes it. */
  close_setup(): Promise<void>;
  /** Forgets what the setup last said went wrong. */
  clear_problem(): Promise<void>;
  /** Asks the bridge's program to have the bot's token put in the person's vault: the person types it once on the system's own card, and the vault keeps it. This account keeps the item's address, never the token. */
  ask_token(): Promise<void>;
  /** Keeps the vault item the person's token was put in: the program's answer to `ask_token`. The item's address is all this record keeps. */
  keep_token(args: {
    ask: string;
    item: string;
  }): Promise<void>;
  /** Finds the servers the bot is in, so one can be chosen: the bridge's program asks Discord with the token the vault keeps. */
  find_servers(): Promise<void>;
  /** The servers finding the bot's servers answered: the program's answer to `find_servers`. */
  found_servers(args: {
    ask: string;
    servers: ServerChoice[];
  }): Promise<void>;
  /** The server the person chose to bring in: the program reads its channels so one of them can be brought in alone. */
  choose_server(args: {
    s: ServerChoice;
  }): Promise<void>;
  /** The server's channels as the program read them: the program's answer to `choose_server`. */
  found_channels(args: {
    ask: string;
    channels: DChannel[];
  }): Promise<void>;
  /** Runs `found_channels` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  found_channelsMany(items: FoundChannelsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Chooses the one channel of the server to bring in, or none for the whole server. */
  choose_channel(args: {
    channel: string;
  }): Promise<void>;
  /** Runs `choose_channel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  choose_channelMany(items: ChooseChannelArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Goes back from the channels to the servers, choosing again. */
  back_to_servers(): Promise<void>;
  /** Runs `back_to_servers` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  back_to_serversMany(items: BackToServersArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Why the setup step `ask` did not work: the program's answer, in words the person reads. */
  setup_failed(args: {
    ask: string;
    problem: string;
  }): Promise<void>;
  /** Binds this account to the server and channels the setup chose. The server's layout stays Discord's until the import ran. */
  bridge_server(): Promise<void>;
  /** Binds this account to one server. The server's layout stays Discord's until the import ran. */
  connect(args: {
    guild_id: string;
    name: string;
    only_channel?: string;
    mirror_channels?: boolean;
    map_roles?: boolean;
  }): Promise<void>;
  /** Records Discord's layout as the latest event reported it: channels and roles the layout no longer lists go, the ones it lists are brought up to date, and new ones come in. */
  observe_layout(args: {
    bot_user: string;
    channels: DChannel[];
    roles: DRole[];
    emojis: DEmoji[];
  }): Promise<void>;
  /** Joins a conversation, message or the like here to its Discord side. */
  map(args: {
    kind: MappedKind;
    tilo: string;
    discord: string;
    origin?: OriginSide;
    persona?: string;
    root?: string;
  }): Promise<void>;
  /** Runs `map` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  mapMany(items: MapArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Forgets a mapping, keeping the two sides apart from now on when asked. */
  unmap(args: {
    kind: MappedKind;
    tilo: string;
    keep_apart?: boolean;
  }): Promise<void>;
  /** Runs `unmap` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  unmapMany(items: UnmapArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Starts signing in with Discord, as the person looking: the bridge's program builds the consent page and the state that binds what comes back to them. */
  connect_account(): Promise<void>;
  /** Where the person goes to sign in with Discord, and the digest of the state that binds what comes back to them: the program's answer to `connect_account`. */
  start_sign_in(args: {
    ask: string;
    url: string;
    sha256: string;
  }): Promise<void>;
  /** Offers a Discord account for linking, from the server: `/link` answered with a one-time token, whose check this is. The offer stands ten minutes, replacing any run out and any other offer kept under the same check. */
  offer_link(args: {
    check: string;
    discord_user: string;
    user_name: string;
    display_name?: string;
    avatar?: string;
    interaction?: string;
  }): Promise<void>;
  /** Runs `offer_link` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  offer_linkMany(items: OfferLinkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Offers the account that signed in on Discord's consent page, to the person who started the round trip `state_sha256` names. */
  offer_sign_in(args: {
    check: string;
    discord_user: string;
    user_name: string;
    state_sha256: string;
    display_name?: string;
    avatar?: string;
  }): Promise<void>;
  /** Runs `offer_sign_in` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  offer_sign_inMany(items: OfferSignInArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Links the Discord account a one-time token offers to the person looking. The offer is used up, and a link the person had before is replaced. */
  complete_link(args: {
    proof: string;
  }): Promise<void>;
  /** Runs `complete_link` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  complete_linkMany(items: CompleteLinkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Offers signing in with Discord: this device's consent page, without its state; empty for none. */
  set_sign_in(args: {
    page: string;
  }): Promise<void>;
  /** Edits a slash command's reply to what came of it. */
  answer_interaction(args: {
    op: AnswerInteractionOp;
  }): Promise<void>;
  /** Takes a linked account's newer names and avatar. */
  rename_link(args: {
    discord_user: string;
    user_name: string;
    display_name?: string;
    avatar?: string;
  }): Promise<void>;
  /** Runs `rename_link` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rename_linkMany(items: RenameLinkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps a linked account's avatar as the person's picture, until they upload their own. */
  set_link_photo(args: {
    discord_user: string;
    avatar: string;
    photo: string;
  }): Promise<void>;
  /** Runs `set_link_photo` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_link_photoMany(items: SetLinkPhotoArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Unlinks a Discord account: its person's, or the bridge's. */
  unlink(args: {
    discord_user: string;
  }): Promise<void>;
  /** Runs `unlink` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  unlinkMany(items: UnlinkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Unlinks the person's own Discord account. */
  unlink_mine(): Promise<void>;
  /** Runs `unlink_mine` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  unlink_mineMany(items: UnlinkMineArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Stops mirroring a Discord account: its messages are no longer carried, and the ones already here are removed. It is not banned there. */
  block_persona(args: {
    discord_user: string;
  }): Promise<void>;
  /** Runs `block_persona` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  block_personaMany(items: BlockPersonaArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Mirrors a Discord account again. */
  unblock_persona(args: {
    discord_user: string;
  }): Promise<void>;
  /** Runs `unblock_persona` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  unblock_personaMany(items: UnblockPersonaArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Remember where an avatar's image is stored. */
  remember_avatar(args: {
    hash: string;
    blob: string;
  }): Promise<void>;
  /** Runs `remember_avatar` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remember_avatarMany(items: RememberAvatarArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Finishes adopting the server: everything in it is here now. */
  finish_import(args: {
    tilo_only: string[];
  }): Promise<void>;
  /** Runs `finish_import` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  finish_importMany(items: FinishImportArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sends a message through a channel's webhook, as the bridge plans it. The call arrives as the JSON the bridge's program writes, and is read as the record its shape declares. */
  send_there(args: {
    op: unknown;
  }): Promise<void>;
  /** Edits a message the bridge sent. */
  edit_there(args: {
    op: unknown;
  }): Promise<void>;
  /** Deletes a message there. */
  delete_there(args: {
    op: unknown;
  }): Promise<void>;
  /** Adds the bot's reaction. */
  react_there(args: {
    op: unknown;
  }): Promise<void>;
  /** Takes the bot's reaction back. */
  unreact_there(args: {
    op: unknown;
  }): Promise<void>;
  /** Starts a thread on a message. */
  start_thread_there(args: {
    op: unknown;
  }): Promise<void>;
  /** Creates a channel's webhook. */
  create_webhook(args: {
    op: unknown;
  }): Promise<void>;
  /** Creates a channel there, or brings it back in line. */
  apply_channel(args: {
    op: ChannelSpec;
  }): Promise<void>;
  /** Deletes a channel there. */
  delete_channel_there(args: {
    channel: string;
  }): Promise<void>;
  /** Puts channels and categories in their places there, so the server lists them in this order. */
  arrange_channels(args: {
    op: ArrangeOp;
  }): Promise<void>;
  /** Says whether the bridge may change the server's channels: mirroring is on and the server is adopted. */
  may_mirror(): Promise<void>;
  /** Says whether the bridge made the Discord channel or category `id` for one here. */
  may_change(args: {
    id: string;
  }): Promise<void>;
  /** Creates a role there, or brings it back in line. */
  apply_role(args: {
    op: unknown;
  }): Promise<void>;
  /** Deletes a role there. */
  delete_role(args: {
    op: DeleteRoleOp;
  }): Promise<void>;
  /** Sets a linked member's roles there. */
  set_member_roles(args: {
    op: MemberRolesOp;
  }): Promise<void>;
  /** Kicks a linked member there. */
  kick(args: {
    op: unknown;
  }): Promise<void>;
  /** Bans a linked member there. */
  ban(args: {
    op: unknown;
  }): Promise<void>;
  /** Unbans a linked member there. */
  unban(args: {
    op: unknown;
  }): Promise<void>;
  /** Sets a channel's permission overwrite, for a role the bridge made. */
  set_overwrite(args: {
    op: unknown;
  }): Promise<void>;
  /** Copies a file from Discord's CDN into the workspace's content store. */
  fetch_asset(args: {
    op: unknown;
  }): Promise<void>;
  /** Uploads a workspace emoji to the server, or renames one the bridge uploaded. */
  apply_emoji(args: {
    op: unknown;
  }): Promise<void>;
  /** Deletes an emoji the bridge uploaded. */
  delete_emoji(args: {
    op: unknown;
  }): Promise<void>;
  /** Records what became of an emoji upload, rename or delete. */
  settle_emoji(args: {
    tilo: string;
    emoji_id?: string;
    problem?: string;
    picture?: string;
    was?: string;
    deleted?: boolean;
  }): Promise<void>;
  /** Says why the bridge cannot carry voice now, or that it can again. */
  report_voice(args: {
    problem: string;
  }): Promise<void>;
  /** A value nothing reads, for the trigger's frame that does nothing itself: the origin polls Discord's events and the conversations here in its own loop. */
  pump(): Promise<void>;
}

/** The actions `Discord` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DISCORD_ACTIONS = ["open_setup", "close_setup", "clear_problem", "ask_token", "keep_token", "find_servers", "found_servers", "choose_server", "found_channels", "found_channelsMany", "choose_channel", "choose_channelMany", "back_to_servers", "back_to_serversMany", "setup_failed", "bridge_server", "connect", "observe_layout", "map", "mapMany", "unmap", "unmapMany", "connect_account", "start_sign_in", "offer_link", "offer_linkMany", "offer_sign_in", "offer_sign_inMany", "complete_link", "complete_linkMany", "set_sign_in", "answer_interaction", "rename_link", "rename_linkMany", "set_link_photo", "set_link_photoMany", "unlink", "unlinkMany", "unlink_mine", "unlink_mineMany", "block_persona", "block_personaMany", "unblock_persona", "unblock_personaMany", "remember_avatar", "remember_avatarMany", "finish_import", "finish_importMany", "send_there", "edit_there", "delete_there", "react_there", "unreact_there", "start_thread_there", "create_webhook", "apply_channel", "delete_channel_there", "arrange_channels", "may_mirror", "may_change", "apply_role", "delete_role", "set_member_roles", "kick", "ban", "unban", "set_overwrite", "fetch_asset", "apply_emoji", "delete_emoji", "settle_emoji", "report_voice", "pump"] as const;

/** The `tl.so.tilo.discord.preview.Discord` kind at `path`. */
export function discord(ws: tilo.Workspace, path: string): Discord {
  return resourceProxy(ws, path, SURFACE, DISCORD_ACTIONS) as unknown as Discord;
}

/** DChannel — `tl.so.tilo.discord.preview.DChannel`. */
export interface DChannel extends tilo.Resource {
  "7p2rou2g": string;  // id
  "5g2es5cu": ChannelKind;  // kind
  i2iwod32: string;  // name
  kj47s5k3: string | null;  // parent
  shpntyiy: number;  // position
  ys7fs4ma: string;  // topic
  "3mwph4ce": Overwrite[];  // overwrites
}

/** The actions `DChannel` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DCHANNEL_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview.DChannel` kind at `path`. */
export function dChannel(ws: tilo.Workspace, path: string): DChannel {
  return resourceProxy(ws, path, SURFACE, DCHANNEL_ACTIONS) as unknown as DChannel;
}

/** Overwrite — `tl.so.tilo.discord.preview.Overwrite`. */
export interface Overwrite extends tilo.Resource {
  x427uoge: string;  // id
  "76fajf53": number;  // kind
  "2yzg4vng": string;  // allow
  dbzgqxgr: string;  // deny
}

/** The actions `Overwrite` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OVERWRITE_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview.Overwrite` kind at `path`. */
export function overwrite(ws: tilo.Workspace, path: string): Overwrite {
  return resourceProxy(ws, path, SURFACE, OVERWRITE_ACTIONS) as unknown as Overwrite;
}

/** DRole — `tl.so.tilo.discord.preview.DRole`. */
export interface DRole extends tilo.Resource {
  xom5hhqp: string;  // id
  aceuzwtj: string;  // name
  wxuu6jgi: string;  // permissions
  pddsqdvg: number;  // color
  xtz54dig: number;  // position
  zmedxuk3: boolean;  // managed
}

/** The actions `DRole` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DROLE_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview.DRole` kind at `path`. */
export function dRole(ws: tilo.Workspace, path: string): DRole {
  return resourceProxy(ws, path, SURFACE, DROLE_ACTIONS) as unknown as DRole;
}

/** DEmoji — `tl.so.tilo.discord.preview.DEmoji`. */
export interface DEmoji extends tilo.Resource {
  oaaqbeyg: string;  // id
  "5vadig7h": string;  // name
  drktwmcm: boolean;  // animated
}

/** The actions `DEmoji` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DEMOJI_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview.DEmoji` kind at `path`. */
export function dEmoji(ws: tilo.Workspace, path: string): DEmoji {
  return resourceProxy(ws, path, SURFACE, DEMOJI_ACTIONS) as unknown as DEmoji;
}

/** Mapping — `tl.so.tilo.discord.preview.Mapping`. */
export interface Mapping extends tilo.Resource {
  ymr43fiv: MappedKind;  // kind
  qt55olnf: string;  // tilo
  vzp2s2xi: string;  // discord
  tjidhwme: OriginSide;  // origin
  ozmgf6kq: string;  // persona
  "4whazn6a": string;  // root
  ctwuv3r6: string;  // picture
  hj3vtcw2: string;  // said
  j2bra6h6: string[];  // emoji
}

/** The actions `Mapping` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MAPPING_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview.Mapping` kind at `path`. */
export function mapping(ws: tilo.Workspace, path: string): Mapping {
  return resourceProxy(ws, path, SURFACE, MAPPING_ACTIONS) as unknown as Mapping;
}

/** Link — `tl.so.tilo.discord.preview.Link`. */
export interface Link extends tilo.Resource {
  shknzkt7: string;  // discord_user
  lvaegbi4: string;  // person
  o4oyz2pi: string;  // linked_at
  wivwsyri: string;  // user_name
  qxx3lhen: string;  // display_name
  i5lrpbng: string;  // avatar
  e2cawszl: string;  // interaction
  bbxol7y7: string;  // photo
  fijk4xyy: string;  // photo_avatar
}

/** The actions `Link` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LINK_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview.Link` kind at `path`. */
export function link(ws: tilo.Workspace, path: string): Link {
  return resourceProxy(ws, path, SURFACE, LINK_ACTIONS) as unknown as Link;
}

/** LinkOffer — `tl.so.tilo.discord.preview.LinkOffer`. */
export interface LinkOffer extends tilo.Resource {
  q4uc376y: string;  // check
  "6xuuyuym": string;  // discord_user
  "3elvjryr": string;  // user_name
  phqjglse: string;  // display_name
  ynydwexj: string;  // avatar
  am2zudro: string;  // person
  umtbptha: string;  // interaction
  lrq3epw3: string;  // expires
}

/** The actions `LinkOffer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LINKOFFER_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview.LinkOffer` kind at `path`. */
export function linkOffer(ws: tilo.Workspace, path: string): LinkOffer {
  return resourceProxy(ws, path, SURFACE, LINKOFFER_ACTIONS) as unknown as LinkOffer;
}

/** Connect — `tl.so.tilo.discord.preview.Connect`. */
export interface Connect extends tilo.Resource {
  "3eaw5xx6": string;  // person
  ljxvxizk: string;  // state_sha256
  jpa253sd: string;  // expires
}

/** The actions `Connect` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CONNECT_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview.Connect` kind at `path`. */
export function connect(ws: tilo.Workspace, path: string): Connect {
  return resourceProxy(ws, path, SURFACE, CONNECT_ACTIONS) as unknown as Connect;
}

/** ServerChoice — `tl.so.tilo.discord.preview.ServerChoice`. */
export interface ServerChoice extends tilo.Resource {
  ymsxjog2: string;  // id
  tafgiv6q: string;  // name
}

/** The actions `ServerChoice` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SERVERCHOICE_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview.ServerChoice` kind at `path`. */
export function serverChoice(ws: tilo.Workspace, path: string): ServerChoice {
  return resourceProxy(ws, path, SURFACE, SERVERCHOICE_ACTIONS) as unknown as ServerChoice;
}

/** SendOp — `tl.so.tilo.discord.preview.SendOp`. */
export interface SendOp extends tilo.Resource {
  "7lcsdnr4": string;  // op
  amt4m7mw: string;  // channel
  "3dhjnhjd": string;  // webhook
  heqsslfh: string | null;  // thread
  fdgzp6on: string;  // username
  qje4kbhs: string | null;  // avatar_url
  nqgp6src: string | null;  // avatar_blob
  d4x237pg: string;  // content
  ffcddfvf: OutAttachment[];  // attachments
  zhzpfsvd: number;  // created_ms
  t3tk2ray: string;  // tilo
}

/** The actions `SendOp` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SENDOP_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview.SendOp` kind at `path`. */
export function sendOp(ws: tilo.Workspace, path: string): SendOp {
  return resourceProxy(ws, path, SURFACE, SENDOP_ACTIONS) as unknown as SendOp;
}

/** OutAttachment — `tl.so.tilo.discord.preview.OutAttachment`. */
export interface OutAttachment extends tilo.Resource {
  u7emqycf: string;  // blob
  zs5m5uxa: string;  // name
  aeq3264s: number;  // size
}

/** The actions `OutAttachment` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OUTATTACHMENT_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview.OutAttachment` kind at `path`. */
export function outAttachment(ws: tilo.Workspace, path: string): OutAttachment {
  return resourceProxy(ws, path, SURFACE, OUTATTACHMENT_ACTIONS) as unknown as OutAttachment;
}

/** EditOp — `tl.so.tilo.discord.preview.EditOp`. */
export interface EditOp extends tilo.Resource {
  "2ksixnn3": string;  // op
  "2sbyakmv": string;  // channel
  bffu3pev: string;  // webhook
  bfdmt6yv: string;  // message
  jb6s4der: string | null;  // thread
  raedlkm5: string;  // content
}

/** The actions `EditOp` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const EDITOP_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview.EditOp` kind at `path`. */
export function editOp(ws: tilo.Workspace, path: string): EditOp {
  return resourceProxy(ws, path, SURFACE, EDITOP_ACTIONS) as unknown as EditOp;
}

/** DeleteOp — `tl.so.tilo.discord.preview.DeleteOp`. */
export interface DeleteOp extends tilo.Resource {
  n5ulezfg: string;  // op
  n2yclrmw: string;  // channel
  liw2ay3q: string | null;  // webhook
  w6a2lwlt: string;  // message
  ojxxdcut: string | null;  // thread
}

/** The actions `DeleteOp` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DELETEOP_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview.DeleteOp` kind at `path`. */
export function deleteOp(ws: tilo.Workspace, path: string): DeleteOp {
  return resourceProxy(ws, path, SURFACE, DELETEOP_ACTIONS) as unknown as DeleteOp;
}

/** ReactOp — `tl.so.tilo.discord.preview.ReactOp`. */
export interface ReactOp extends tilo.Resource {
  juyzi4a5: string;  // op
  me76z7t7: string;  // channel
  orzlvcjf: string;  // message
  mhrysspc: string;  // emoji
}

/** The actions `ReactOp` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REACTOP_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview.ReactOp` kind at `path`. */
export function reactOp(ws: tilo.Workspace, path: string): ReactOp {
  return resourceProxy(ws, path, SURFACE, REACTOP_ACTIONS) as unknown as ReactOp;
}

/** StartThreadOp — `tl.so.tilo.discord.preview.StartThreadOp`. */
export interface StartThreadOp extends tilo.Resource {
  eqdu4s5n: string;  // op
  qxyl2yhx: string;  // channel
  "2g4xqyvu": string;  // message
  "2ba4r76l": string;  // name
  "63ujdmb5": string;  // tilo
}

/** The actions `StartThreadOp` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const STARTTHREADOP_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview.StartThreadOp` kind at `path`. */
export function startThreadOp(ws: tilo.Workspace, path: string): StartThreadOp {
  return resourceProxy(ws, path, SURFACE, STARTTHREADOP_ACTIONS) as unknown as StartThreadOp;
}

/** CreateWebhookOp — `tl.so.tilo.discord.preview.CreateWebhookOp`. */
export interface CreateWebhookOp extends tilo.Resource {
  p3acbhsm: string;  // op
  gu2hv6zx: string;  // channel
  ijnwz6yd: string;  // tilo
}

/** The actions `CreateWebhookOp` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CREATEWEBHOOKOP_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview.CreateWebhookOp` kind at `path`. */
export function createWebhookOp(ws: tilo.Workspace, path: string): CreateWebhookOp {
  return resourceProxy(ws, path, SURFACE, CREATEWEBHOOKOP_ACTIONS) as unknown as CreateWebhookOp;
}

/** ChannelSpec — `tl.so.tilo.discord.preview.ChannelSpec`. */
export interface ChannelSpec extends tilo.Resource {
  aid6dwih: string;  // op
  nyneqhtp: string | null;  // id
  "344fcxyq": string;  // tilo
  "7qc7rtaa": ChannelKind;  // kind
  z5ye67p5: string;  // name
  ghxsuztc: string;  // topic
  yniihhyc: string | null;  // parent
  fvy2cnhd: number;  // position
}

/** The actions `ChannelSpec` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CHANNELSPEC_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview.ChannelSpec` kind at `path`. */
export function channelSpec(ws: tilo.Workspace, path: string): ChannelSpec {
  return resourceProxy(ws, path, SURFACE, CHANNELSPEC_ACTIONS) as unknown as ChannelSpec;
}

/** ChannelDeletion — `tl.so.tilo.discord.preview.ChannelDeletion`. */
export interface ChannelDeletion extends tilo.Resource {
  fvyrqodk: string;  // op
  rhmxfpcn: string;  // channel
}

/** The actions `ChannelDeletion` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CHANNELDELETION_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview.ChannelDeletion` kind at `path`. */
export function channelDeletion(ws: tilo.Workspace, path: string): ChannelDeletion {
  return resourceProxy(ws, path, SURFACE, CHANNELDELETION_ACTIONS) as unknown as ChannelDeletion;
}

/** CategoryPlace — `tl.so.tilo.discord.preview.CategoryPlace`. */
export interface CategoryPlace extends tilo.Resource {
  n3kocqem: string;  // category
  tjug6eo2: number;  // position
}

/** The actions `CategoryPlace` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CATEGORYPLACE_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview.CategoryPlace` kind at `path`. */
export function categoryPlace(ws: tilo.Workspace, path: string): CategoryPlace {
  return resourceProxy(ws, path, SURFACE, CATEGORYPLACE_ACTIONS) as unknown as CategoryPlace;
}

/** ChannelPlace — `tl.so.tilo.discord.preview.ChannelPlace`. */
export interface ChannelPlace extends tilo.Resource {
  pposs7uz: string;  // channel
  w7v7vvgk: string | null;  // parent
  rmnsxntg: number;  // position
}

/** The actions `ChannelPlace` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CHANNELPLACE_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview.ChannelPlace` kind at `path`. */
export function channelPlace(ws: tilo.Workspace, path: string): ChannelPlace {
  return resourceProxy(ws, path, SURFACE, CHANNELPLACE_ACTIONS) as unknown as ChannelPlace;
}

/** ArrangeOp — `tl.so.tilo.discord.preview.ArrangeOp`. */
export interface ArrangeOp extends tilo.Resource {
  "4ebcu3vj": string;  // op
  vamfb4ci: CategoryPlace[];  // categories
  "2jxptmbo": ChannelPlace[];  // channels
}

/** The actions `ArrangeOp` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ARRANGEOP_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview.ArrangeOp` kind at `path`. */
export function arrangeOp(ws: tilo.Workspace, path: string): ArrangeOp {
  return resourceProxy(ws, path, SURFACE, ARRANGEOP_ACTIONS) as unknown as ArrangeOp;
}

/** RoleSpec — `tl.so.tilo.discord.preview.RoleSpec`. */
export interface RoleSpec extends tilo.Resource {
  oad2kmtd: string;  // op
  "6khaatsw": string | null;  // id
  "6fif6ct6": string;  // tilo
  x53hs7jf: string;  // name
  vmrvsfvk: string;  // permissions
  w2v4ktgb: number;  // color
}

/** The actions `RoleSpec` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ROLESPEC_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview.RoleSpec` kind at `path`. */
export function roleSpec(ws: tilo.Workspace, path: string): RoleSpec {
  return resourceProxy(ws, path, SURFACE, ROLESPEC_ACTIONS) as unknown as RoleSpec;
}

/** DeleteRoleOp — `tl.so.tilo.discord.preview.DeleteRoleOp`. */
export interface DeleteRoleOp extends tilo.Resource {
  wxxprqy5: string;  // op
  "43gurfsq": string;  // role
}

/** The actions `DeleteRoleOp` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DELETEROLEOP_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview.DeleteRoleOp` kind at `path`. */
export function deleteRoleOp(ws: tilo.Workspace, path: string): DeleteRoleOp {
  return resourceProxy(ws, path, SURFACE, DELETEROLEOP_ACTIONS) as unknown as DeleteRoleOp;
}

/** MemberRolesOp — `tl.so.tilo.discord.preview.MemberRolesOp`. */
export interface MemberRolesOp extends tilo.Resource {
  kudwdhlw: string;  // op
  auupacru: string;  // user
  n3pur4nd: string[];  // add
  xgavnnvp: string[];  // remove
}

/** The actions `MemberRolesOp` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MEMBERROLESOP_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview.MemberRolesOp` kind at `path`. */
export function memberRolesOp(ws: tilo.Workspace, path: string): MemberRolesOp {
  return resourceProxy(ws, path, SURFACE, MEMBERROLESOP_ACTIONS) as unknown as MemberRolesOp;
}

/** MemberOp — `tl.so.tilo.discord.preview.MemberOp`. */
export interface MemberOp extends tilo.Resource {
  vpb4svn2: string;  // op
  w6fejnzs: string;  // user
  lwbm7m7g: string;  // reason
}

/** The actions `MemberOp` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MEMBEROP_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview.MemberOp` kind at `path`. */
export function memberOp(ws: tilo.Workspace, path: string): MemberOp {
  return resourceProxy(ws, path, SURFACE, MEMBEROP_ACTIONS) as unknown as MemberOp;
}

/** OverwriteOp — `tl.so.tilo.discord.preview.OverwriteOp`. */
export interface OverwriteOp extends tilo.Resource {
  hoy7opkc: string;  // op
  "5bwuxkgk": string;  // channel
  erieg3ch: string;  // target
  cgrybxyv: OverwriteTarget;  // target_kind
  wmuwv3mm: string;  // allow
  iec2uwfy: string;  // deny
}

/** The actions `OverwriteOp` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OVERWRITEOP_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview.OverwriteOp` kind at `path`. */
export function overwriteOp(ws: tilo.Workspace, path: string): OverwriteOp {
  return resourceProxy(ws, path, SURFACE, OVERWRITEOP_ACTIONS) as unknown as OverwriteOp;
}

/** FetchAssetOp — `tl.so.tilo.discord.preview.FetchAssetOp`. */
export interface FetchAssetOp extends tilo.Resource {
  n5gkbaj7: string;  // op
  dymgc2vl: string;  // url
  grilu5ek: number;  // max_bytes
  "4xznvc4c": boolean;  // inline
}

/** The actions `FetchAssetOp` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FETCHASSETOP_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview.FetchAssetOp` kind at `path`. */
export function fetchAssetOp(ws: tilo.Workspace, path: string): FetchAssetOp {
  return resourceProxy(ws, path, SURFACE, FETCHASSETOP_ACTIONS) as unknown as FetchAssetOp;
}

/** AnswerInteractionOp — `tl.so.tilo.discord.preview.AnswerInteractionOp`. */
export interface AnswerInteractionOp extends tilo.Resource {
  noppx5yi: string;  // op
  ecw6352r: string;  // interaction
  ehjwcbzo: string;  // content
}

/** The actions `AnswerInteractionOp` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ANSWERINTERACTIONOP_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview.AnswerInteractionOp` kind at `path`. */
export function answerInteractionOp(ws: tilo.Workspace, path: string): AnswerInteractionOp {
  return resourceProxy(ws, path, SURFACE, ANSWERINTERACTIONOP_ACTIONS) as unknown as AnswerInteractionOp;
}

/** EmojiSpec — `tl.so.tilo.discord.preview.EmojiSpec`. */
export interface EmojiSpec extends tilo.Resource {
  ty6f2nw2: string;  // op
  ttzu4a2l: string | null;  // id
  bunzoexc: string;  // name
  "7beod5ga": string;  // picture
  vmrzdpp2: string;  // tilo
}

/** The actions `EmojiSpec` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const EMOJISPEC_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview.EmojiSpec` kind at `path`. */
export function emojiSpec(ws: tilo.Workspace, path: string): EmojiSpec {
  return resourceProxy(ws, path, SURFACE, EMOJISPEC_ACTIONS) as unknown as EmojiSpec;
}

/** DeleteEmojiOp — `tl.so.tilo.discord.preview.DeleteEmojiOp`. */
export interface DeleteEmojiOp extends tilo.Resource {
  w7bai4fx: string;  // op
  h2uwhtax: string;  // emoji
  "3ze5unut": string;  // tilo
}

/** The actions `DeleteEmojiOp` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DELETEEMOJIOP_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview.DeleteEmojiOp` kind at `path`. */
export function deleteEmojiOp(ws: tilo.Workspace, path: string): DeleteEmojiOp {
  return resourceProxy(ws, path, SURFACE, DELETEEMOJIOP_ACTIONS) as unknown as DeleteEmojiOp;
}

/** Accounts — `tl.so.tilo.discord.preview.Accounts`. */
export interface Accounts extends tilo.Resource {
  z7et5ezu: Array<tilo.Entry<Discord>>;  // accounts
  /** Starts the account: the record the setup writes into. */
  begin_setup(): Promise<Discord>;
}

/** The actions `Accounts` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ACCOUNTS_ACTIONS = ["begin_setup"] as const;

/** The `tl.so.tilo.discord.preview.Accounts` kind at `path`. */
export function accounts(ws: tilo.Workspace, path: string): Accounts {
  return resourceProxy(ws, path, SURFACE, ACCOUNTS_ACTIONS) as unknown as Accounts;
}

/** VoicePair — `tl.so.tilo.discord.preview.VoicePair`. */
export interface VoicePair extends tilo.Resource {
  "5xed4gep": string;  // discord
  "5pvw2ica": string;  // tilo
}

/** The actions `VoicePair` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const VOICEPAIR_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview.VoicePair` kind at `path`. */
export function voicePair(ws: tilo.Workspace, path: string): VoicePair {
  return resourceProxy(ws, path, SURFACE, VOICEPAIR_ACTIONS) as unknown as VoicePair;
}

/** Discord — `tl.so.tilo.discord.preview`. */
export interface Preview extends tilo.Resource {
}

/** The actions `Preview` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PREVIEW_ACTIONS = [] as const;

/** The `tl.so.tilo.discord.preview` kind at `path`. */
export function preview(ws: tilo.Workspace, path: string): Preview {
  return resourceProxy(ws, path, SURFACE, PREVIEW_ACTIONS) as unknown as Preview;
}

