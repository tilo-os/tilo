/**
 * Types for YouTube (`so.tilo.tube`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`tube/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.tube@8213a8f9a9dcf15a";

/** The arguments of `subscribe`. */
export interface SubscribeArgs {
  subscribed?: boolean;
}

/** The arguments of `toggle_subscription`. */
export interface ToggleSubscriptionArgs {
}

/** The arguments of `like`. */
export interface LikeArgs {
  liked?: boolean;
}

/** The arguments of `watch_later`. */
export interface WatchLaterArgs {
  saved?: boolean;
}

/** The arguments of `forget`. */
export interface ForgetArgs {
}

/** The arguments of `retract_reply`. */
export interface RetractReplyArgs {
}

/** The arguments of `play_items`. */
export interface PlayItemsArgs {
  videos: Video[];
  at: string;
}

/** The arguments of `delete_history`. */
export interface DeleteHistoryArgs {
}

/** The arguments of `set_autoplay`. */
export interface SetAutoplayArgs {
  on: boolean;
}

/** The arguments of `set_quality`. */
export interface SetQualityArgs {
  quality: string;
}

/** The arguments of `set_sponsorblock`. */
export interface SetSponsorblockArgs {
  on: boolean;
}

/** The arguments of `set_dearrow`. */
export interface SetDearrowArgs {
  on: boolean;
}

/** The arguments of `set_dislikes`. */
export interface SetDislikesArgs {
  on: boolean;
}

/** The arguments of `set_comments`. */
export interface SetCommentsArgs {
  on: boolean;
}

/** The arguments of `set_speed`. */
export interface SetSpeedArgs {
  speed: number;
}

/** The arguments of `pair_device`. */
export interface PairDeviceArgs {
  invite: string;
}

/** The arguments of `approve_device`. */
export interface ApproveDeviceArgs {
  device: PairedDevice;
}

/** The arguments of `revoke_device`. */
export interface RevokeDeviceArgs {
  device: PairedDevice;
}

/** The arguments of `sync_devices`. */
export interface SyncDevicesArgs {
}

/** The arguments of `auto_sync`. */
export interface AutoSyncArgs {
}

/** The arguments of `grayjay_ended`. */
export interface GrayjayEndedArgs {
  j: unknown;
}

/** The arguments of `keep_paired`. */
export interface KeepPairedArgs {
  answer: string;
}

/** The arguments of `keep_exchange`. */
export interface KeepExchangeArgs {
  answer: string;
}

/** The arguments of `filter_search`. */
export interface FilterSearchArgs {
  kind: string;
}

/** The arguments of `end_search`. */
export interface EndSearchArgs {
}

/** The arguments of `like_comment`. */
export interface LikeCommentArgs {
  c: Comment;
  liked?: boolean;
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

/** Channel — `tl.so.tilo.tube.Channel`. */
export interface Channel extends tilo.Resource {
  "26ctwf34": string;  // key
  ya2u4cw6: string;  // name
  woyix7h2: string;  // handle
  ah5nkqv7: number;  // subscribers
  "2qxzpxur": string;  // avatar
  d4mzmvrr: string;  // banner
  rzhtvjsv: string;  // description
  lfopp7e7: boolean;  // verified
  "2ap24kjm": string;  // found_for
  /** Subscribes to the channel, or unsubscribes: the button says which it will do. */
  subscribe(args?: {
    subscribed?: boolean;
  }): Promise<void>;
  /** Runs `subscribe` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  subscribeMany(items: SubscribeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Subscribes to the channel, or takes the subscription back. */
  toggle_subscription(): Promise<void>;
  /** Runs `toggle_subscription` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  toggle_subscriptionMany(items: ToggleSubscriptionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks the service for the channel's videos. */
  refresh_channel(): Promise<void>;
}

/** The actions `Channel` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CHANNEL_ACTIONS = ["subscribe", "subscribeMany", "toggle_subscription", "toggle_subscriptionMany", "refresh_channel"] as const;

/** The `tl.so.tilo.tube.Channel` kind at `path`. */
export function channel(ws: tilo.Workspace, path: string): Channel {
  return resourceProxy(ws, path, SURFACE, CHANNEL_ACTIONS) as unknown as Channel;
}

/** Video — `tl.so.tilo.tube.Video`. */
export interface Video extends tilo.Resource {
  irzy3c7m: string;  // key
  fpxk5w7b: string;  // title
  mhkply6c: string;  // channel_key
  "2mklwbfq": string;  // channel_name
  lfky4erv: string;  // channel_avatar
  txtos5p6: number;  // views
  "2ruyhh5f": string;  // published
  mvlf65b5: number;  // duration
  pfdoae2w: string;  // description
  ddfiordt: number;  // likes
  lhbq7qff: number;  // dislikes
  l25oorez: string;  // thumbnail
  vvxj4w64: string;  // dearrow_title
  imamvrnr: string;  // dearrow_thumbnail
  pkbzjs7l: string;  // stream
  jfam7tjf: boolean;  // live
  bifohbpv: boolean;  // liked
  "4mojz3hw": boolean;  // trending
  "5o5fpqgh": string;  // found_for
  /** Likes the video, or takes the like back. */
  like(args?: {
    liked?: boolean;
  }): Promise<void>;
  /** Runs `like` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  likeMany(items: LikeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts the video on the workspace's watch queue, or takes it off. A video put on it goes after the ones already waiting, so the queue plays in the order people added to it. */
  watch_later(args?: {
    saved?: boolean;
  }): Promise<void>;
  /** Runs `watch_later` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  watch_laterMany(items: WatchLaterArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Plays the video, whether or not it was played before: the session takes it as its one item, so the player, the live card and the media keys all act on it, and it continues where the person left it. */
  play(): Promise<void>;
  /** Plays the video from where the person left off, when they did not finish it. */
  resume(): Promise<void>;
  /** Plays the video at `quality`, from where it stands: the player's own quality menu, which asks the service for a stream of that picture. The session keeps the item and the place it stood at, so the picture changes without the playhead moving; the address YouTube answers with is the one the player opens next, and `progress` writes it into the session's item. */
  play_at_quality(args: {
    quality: string;
  }): Promise<void>;
  /** Records how far into the video the person is, and that they watched it now: it moves to the front of the history, and asks the service for the address its bytes play from when that is not known yet. Starting a video, resuming it and moving the playhead as it plays are one action, so a video has one place it is played from. */
  progress(args: {
    at: number;
  }): Promise<void>;
  /** Skips the SponsorBlock segment the video plays into, at `at` (a share of its length), when the person skips segments: the playhead moves to the segment's end, so a sponsor read or an outro is never watched. The player reports where it got to as it plays, so this is what makes a marked segment skip on its own. */
  skip_segment(args: {
    at: number;
  }): Promise<void>;
  /** Takes one video out of the person's own history. */
  forget(): Promise<void>;
  /** Runs `forget` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  forgetMany(items: ForgetArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks the service for where the video's bytes play from again, after the player could not show them: the address the client holds is the service's alone to write, and the answer it gives now is what the player opens. The session moves to where the stream stopped, so the fresh address continues there. */
  retry_playback(args: {
    at: number;
  }): Promise<void>;
  /** Asks the service for the video's comments again, for what people said since the page opened. */
  refresh_comments(): Promise<void>;
}

/** The actions `Video` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const VIDEO_ACTIONS = ["like", "likeMany", "watch_later", "watch_laterMany", "play", "resume", "play_at_quality", "progress", "skip_segment", "forget", "forgetMany", "retry_playback", "refresh_comments"] as const;

/** The `tl.so.tilo.tube.Video` kind at `path`. */
export function video(ws: tilo.Workspace, path: string): Video {
  return resourceProxy(ws, path, SURFACE, VIDEO_ACTIONS) as unknown as Video;
}

/** Playlist — `tl.so.tilo.tube.Playlist`. */
export interface Playlist extends tilo.Resource {
  ow7glfos: string;  // key
  ljyc7mx2: string;  // title
  bzfmd6pg: string;  // channel_key
  zvfpfva3: string;  // channel_name
  "6cllrl7x": number;  // video_count
  "4xvzshd7": string;  // thumbnail
  dge65utd: string[];  // video_keys
  p3yw6p2h: string;  // found_for
  /** Asks the service for what the playlist holds, in its maker's order. */
  refresh_playlist(): Promise<void>;
  /** Plays the playlist from its first video, and the rest in its maker's order. */
  play_playlist(): Promise<void>;
}

/** The actions `Playlist` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PLAYLIST_ACTIONS = ["refresh_playlist", "play_playlist"] as const;

/** The `tl.so.tilo.tube.Playlist` kind at `path`. */
export function playlist(ws: tilo.Workspace, path: string): Playlist {
  return resourceProxy(ws, path, SURFACE, PLAYLIST_ACTIONS) as unknown as Playlist;
}

/** Comment — `tl.so.tilo.tube.Comment`. */
export interface Comment extends tilo.Resource {
  pv4laajx: string;  // key
  li4xiefh: string;  // video_key
  "3ncdj3up": string;  // author
  lgtlgzui: string;  // avatar
  az6iwuma: string;  // text
  hgdwjwgt: number;  // likes
  dninsxc6: string;  // published
  tmf5hovn: boolean;  // pinned
  gp3basiz: boolean;  // hearted
  ha6vpfhz: number;  // replies
  /** Replies to a comment: the workspace keeps the reply itself, beside the comment it answers, since YouTube lets no app post as the person. Everyone who reads the comment reads the reply. */
  reply_to_comment(args: {
    text: string;
  }): Promise<Reply>;
}

/** The actions `Comment` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const COMMENT_ACTIONS = ["reply_to_comment"] as const;

/** The `tl.so.tilo.tube.Comment` kind at `path`. */
export function comment(ws: tilo.Workspace, path: string): Comment {
  return resourceProxy(ws, path, SURFACE, COMMENT_ACTIONS) as unknown as Comment;
}

/** Segment — `tl.so.tilo.tube.Segment`. */
export interface Segment extends tilo.Resource {
  uakovynu: string;  // key
  pvlf5o2c: string;  // video_key
  qlgpeq35: string;  // category
  bbnmvcim: number;  // start
  hts3jgqy: number;  // end
  j7wuhxg5: number;  // votes
}

/** The actions `Segment` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SEGMENT_ACTIONS = [] as const;

/** The `tl.so.tilo.tube.Segment` kind at `path`. */
export function segment(ws: tilo.Workspace, path: string): Segment {
  return resourceProxy(ws, path, SURFACE, SEGMENT_ACTIONS) as unknown as Segment;
}

/** Reply — `tl.so.tilo.tube.Reply`. */
export interface Reply extends tilo.Resource {
  y4coaxks: string;  // comment
  kwxg57v6: string;  // video
  "6psi22ua": string;  // text
  nzwksffp: string | null;  // member
  k3jskmid: string;  // said
  /** Takes back a reply the person wrote. A reply the native app kept is the workspace's, and any member may take it back. */
  retract_reply(): Promise<void>;
  /** Runs `retract_reply` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  retract_replyMany(items: RetractReplyArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Reply` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REPLY_ACTIONS = ["retract_reply", "retract_replyMany"] as const;

/** The `tl.so.tilo.tube.Reply` kind at `path`. */
export function reply(ws: tilo.Workspace, path: string): Reply {
  return resourceProxy(ws, path, SURFACE, REPLY_ACTIONS) as unknown as Reply;
}

/** Subscription — `tl.so.tilo.tube.Subscription`. */
export interface Subscription extends tilo.Resource {
  slhmwv67: string;  // key
  ynf67l6e: string;  // name
}

/** The actions `Subscription` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SUBSCRIPTION_ACTIONS = [] as const;

/** The `tl.so.tilo.tube.Subscription` kind at `path`. */
export function subscription(ws: tilo.Workspace, path: string): Subscription {
  return resourceProxy(ws, path, SURFACE, SUBSCRIPTION_ACTIONS) as unknown as Subscription;
}

/** QueuedVideo — `tl.so.tilo.tube.QueuedVideo`. */
export interface QueuedVideo extends tilo.Resource {
  o3xolmld: string;  // key
}

/** The actions `QueuedVideo` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const QUEUEDVIDEO_ACTIONS = [] as const;

/** The `tl.so.tilo.tube.QueuedVideo` kind at `path`. */
export function queuedVideo(ws: tilo.Workspace, path: string): QueuedVideo {
  return resourceProxy(ws, path, SURFACE, QUEUEDVIDEO_ACTIONS) as unknown as QueuedVideo;
}

/** PlaybackPrefs — `tl.so.tilo.tube.PlaybackPrefs`. */
export interface PlaybackPrefs extends tilo.Resource {
  "5twttwwn": string;  // member
  vze2yftu: boolean;  // autoplay
  lesyml4z: string;  // quality
  vbbcsaos: boolean;  // sponsorblock
  "7cyrtrv6": boolean;  // dearrow
  eyoonq3d: boolean;  // dislikes
  lumd7e36: boolean;  // comments
  t2l6coaj: number;  // speed
}

/** The actions `PlaybackPrefs` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PLAYBACKPREFS_ACTIONS = [] as const;

/** The `tl.so.tilo.tube.PlaybackPrefs` kind at `path`. */
export function playbackPrefs(ws: tilo.Workspace, path: string): PlaybackPrefs {
  return resourceProxy(ws, path, SURFACE, PLAYBACKPREFS_ACTIONS) as unknown as PlaybackPrefs;
}

/** WatchEntry — `tl.so.tilo.tube.WatchEntry`. */
export interface WatchEntry extends tilo.Resource {
  noppuxpb: string;  // key
  stuii7l7: string;  // title
  j37y2bbw: string;  // channel_name
  "7mtvj3xd": string | null;  // member
  dmm6f63t: string;  // watched
  omff5ciz: number;  // progress
  rqajp43d: number;  // left_ms
}

/** The actions `WatchEntry` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WATCHENTRY_ACTIONS = [] as const;

/** The `tl.so.tilo.tube.WatchEntry` kind at `path`. */
export function watchEntry(ws: tilo.Workspace, path: string): WatchEntry {
  return resourceProxy(ws, path, SURFACE, WATCHENTRY_ACTIONS) as unknown as WatchEntry;
}

/** PairedDevice — `tl.so.tilo.tube.PairedDevice`. */
export interface PairedDevice extends tilo.Resource {
  eqjnywdb: string;  // key
  xzt7hvjd: string;  // name
  r6hetdhx: string[];  // addresses
  qshipefo: number;  // port
  rb3itb6c: boolean;  // approved
  dtjxddej: boolean;  // revoked
  dtxc67xp: string;  // approved_at
  "3rbysfau": string;  // last_synced
  ovgswiz5: string;  // last_error
}

/** The actions `PairedDevice` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PAIREDDEVICE_ACTIONS = [] as const;

/** The `tl.so.tilo.tube.PairedDevice` kind at `path`. */
export function pairedDevice(ws: tilo.Workspace, path: string): PairedDevice {
  return resourceProxy(ws, path, SURFACE, PAIREDDEVICE_ACTIONS) as unknown as PairedDevice;
}

/** GrayjayPairAsk — `tl.so.tilo.tube.GrayjayPairAsk`. */
export interface GrayjayPairAsk extends tilo.Resource {
  vjm7bxs2: string;  // invite
}

/** The actions `GrayjayPairAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const GRAYJAYPAIRASK_ACTIONS = [] as const;

/** The `tl.so.tilo.tube.GrayjayPairAsk` kind at `path`. */
export function grayjayPairAsk(ws: tilo.Workspace, path: string): GrayjayPairAsk {
  return resourceProxy(ws, path, SURFACE, GRAYJAYPAIRASK_ACTIONS) as unknown as GrayjayPairAsk;
}

/** GrayjaySyncAsk — `tl.so.tilo.tube.GrayjaySyncAsk`. */
export interface GrayjaySyncAsk extends tilo.Resource {
  ldigr4fm: GrayjayPeer[];  // peers
  "2ubtnp7s": GrayjaySubscription[];  // subscriptions
}

/** The actions `GrayjaySyncAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const GRAYJAYSYNCASK_ACTIONS = [] as const;

/** The `tl.so.tilo.tube.GrayjaySyncAsk` kind at `path`. */
export function grayjaySyncAsk(ws: tilo.Workspace, path: string): GrayjaySyncAsk {
  return resourceProxy(ws, path, SURFACE, GRAYJAYSYNCASK_ACTIONS) as unknown as GrayjaySyncAsk;
}

/** GrayjayPeer — `tl.so.tilo.tube.GrayjayPeer`. */
export interface GrayjayPeer extends tilo.Resource {
  "73ossitw": string;  // key
  u7p5waf7: string;  // name
  eqe3dx4x: string[];  // addresses
  b5mgdh6x: number;  // port
}

/** The actions `GrayjayPeer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const GRAYJAYPEER_ACTIONS = [] as const;

/** The `tl.so.tilo.tube.GrayjayPeer` kind at `path`. */
export function grayjayPeer(ws: tilo.Workspace, path: string): GrayjayPeer {
  return resourceProxy(ws, path, SURFACE, GRAYJAYPEER_ACTIONS) as unknown as GrayjayPeer;
}

/** GrayjaySubscription — `tl.so.tilo.tube.GrayjaySubscription`. */
export interface GrayjaySubscription extends tilo.Resource {
  "5k7gvetf": string;  // url
  o4wrklpp: string;  // name
}

/** The actions `GrayjaySubscription` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const GRAYJAYSUBSCRIPTION_ACTIONS = [] as const;

/** The `tl.so.tilo.tube.GrayjaySubscription` kind at `path`. */
export function grayjaySubscription(ws: tilo.Workspace, path: string): GrayjaySubscription {
  return resourceProxy(ws, path, SURFACE, GRAYJAYSUBSCRIPTION_ACTIONS) as unknown as GrayjaySubscription;
}

/** GrayjayJob — `tl.so.tilo.tube.GrayjayJob`. */
export interface GrayjayJob extends tilo.Resource {
  "4nyda52b": string;  // state
  c7tscegn: string;  // answer
  tshqcr4u: string;  // error
}

/** The actions `GrayjayJob` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const GRAYJAYJOB_ACTIONS = [] as const;

/** The `tl.so.tilo.tube.GrayjayJob` kind at `path`. */
export function grayjayJob(ws: tilo.Workspace, path: string): GrayjayJob {
  return resourceProxy(ws, path, SURFACE, GRAYJAYJOB_ACTIONS) as unknown as GrayjayJob;
}

/** GrayjayOutcome — `tl.so.tilo.tube.GrayjayOutcome`. */
export interface GrayjayOutcome extends tilo.Resource {
  xil726fb: string;  // key
  zkmfyff4: boolean;  // ok
  mp7b4zg2: string;  // error
}

/** The actions `GrayjayOutcome` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const GRAYJAYOUTCOME_ACTIONS = [] as const;

/** The `tl.so.tilo.tube.GrayjayOutcome` kind at `path`. */
export function grayjayOutcome(ws: tilo.Workspace, path: string): GrayjayOutcome {
  return resourceProxy(ws, path, SURFACE, GRAYJAYOUTCOME_ACTIONS) as unknown as GrayjayOutcome;
}

/** GrayjayRead — `tl.so.tilo.tube.GrayjayRead`. */
export interface GrayjayRead extends tilo.Resource {
  oow4nbb2: GrayjayOutcome[];  // peers
  "2vaamao3": GrayjaySubscription[];  // subscriptions
  iddsonzh: string[];  // history
}

/** The actions `GrayjayRead` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const GRAYJAYREAD_ACTIONS = [] as const;

/** The `tl.so.tilo.tube.GrayjayRead` kind at `path`. */
export function grayjayRead(ws: tilo.Workspace, path: string): GrayjayRead {
  return resourceProxy(ws, path, SURFACE, GRAYJAYREAD_ACTIONS) as unknown as GrayjayRead;
}

/** YouTube — `tl.so.tilo.tube.YouTube`. */
export interface YouTube extends tilo.Resource {
  olrung72: string;  // title
  ihhzrm3f: Array<tilo.Entry<Channel>>;  // channels
  qnfjkti5: Array<tilo.Entry<Video>>;  // videos
  v5sh2bpr: Array<tilo.Entry<Playlist>>;  // playlists
  iputtmua: Array<tilo.Entry<Subscription>>;  // subscriptions
  jgjqgesf: Array<tilo.Entry<QueuedVideo>>;  // queue
  hv25j74h: Array<tilo.Entry<WatchEntry>>;  // history
  puten5ar: boolean;  // history_cleared
  ugv4b6is: Array<tilo.Entry<Comment>>;  // comments
  cjlgivfs: Array<tilo.Entry<Segment>>;  // segments
  "5oxdbju7": string[];  // liked_comments
  iphlnbkx: Array<tilo.Entry<Reply>>;  // replies
  epgpppzy: Array<tilo.Entry<PlaybackPrefs>>;  // playback
  f5zuqdsr: unknown | null;  // playing
  "5hk5vbw4": Array<tilo.Entry<unknown>>;  // sessions
  bqesicxq: string;  // search_text
  "2pryeqkt": string;  // search_kind
  egducuxn: string;  // search_length
  ypapv6iy: string;  // search_published
  hcmy6dp7: string;  // search_state
  zjw3anaz: string;  // search_trouble
  nnvco6w3: string;  // feed_state
  r3ksw6bn: string;  // trouble
  qv7t2mza: boolean;  // online
  dmc6hhdb: Array<tilo.Entry<PairedDevice>>;  // devices
  bnycbpqz: string;  // sync_working
  "5siq65eg": string;  // sync_job
  drdwq4ha: string;  // sync_notice
  rx6sx3au: string;  // sync_trouble
  slqvuard: boolean;  // sync_started
  /** Plays the queue from its oldest video: what was put on it first plays first, and the rest follow in that order, so autoplay goes on down the queue. */
  play_queue(): Promise<void>;
  /** The session this app plays through: the one it has, or one made now. It is kept both in `playing`, which a view reads, and in `sessions`, so the record is a resource of its own. */
  session(): Promise<unknown>;
  /** Plays `videos` in the session, beginning with `at`'s place: what plays and what follows, so a shared next, the live card and another device move through the same list. The video at `at` continues where it was left. */
  play_items(args: {
    videos: Video[];
    at: string;
  }): Promise<void>;
  /** Runs `play_items` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  play_itemsMany(items: PlayItemsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** When the video reaches its end and the person asked for the next one, the session moves on to what follows it: the queue's next video is watched now, so its place in the history is kept and its bytes are asked for when the service has not answered with them yet. */
  autoplay_next(args: {
    p: unknown;
    share: number;
  }): Promise<void>;
  /** Deletes the person's own watch history, and the entries carried from before it recorded members, which this history shows them; nobody else's. */
  delete_history(): Promise<void>;
  /** Runs `delete_history` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_historyMany(items: DeleteHistoryArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Each of these keeps one of the person's own playback preferences: the record is theirs, made the first time they set one, and every other member reads their own. */
  set_autoplay(args: {
    on: boolean;
  }): Promise<void>;
  /** Runs `set_autoplay` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_autoplayMany(items: SetAutoplayArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The tallest picture the person wants, as YouTube names it; `auto` takes the best that fits. */
  set_quality(args: {
    quality: string;
  }): Promise<void>;
  /** Runs `set_quality` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_qualityMany(items: SetQualityArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Whether the player skips the segments SponsorBlock marks, as they play. */
  set_sponsorblock(args: {
    on: boolean;
  }): Promise<void>;
  /** Runs `set_sponsorblock` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_sponsorblockMany(items: SetSponsorblockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Whether a video shows the community's title and picture where DeArrow has them. */
  set_dearrow(args: {
    on: boolean;
  }): Promise<void>;
  /** Runs `set_dearrow` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_dearrowMany(items: SetDearrowArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Whether a video shows how many people disliked it, as Return YouTube Dislike estimates it. */
  set_dislikes(args: {
    on: boolean;
  }): Promise<void>;
  /** Runs `set_dislikes` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_dislikesMany(items: SetDislikesArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Whether a video's page shows its comments. */
  set_comments(args: {
    on: boolean;
  }): Promise<void>;
  /** Runs `set_comments` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_commentsMany(items: SetCommentsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** How fast the person watches: every video here starts at this speed, on every device. */
  set_speed(args: {
    speed: number;
  }): Promise<void>;
  /** Runs `set_speed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_speedMany(items: SetSpeedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Pairs a Grayjay device from the invite its Sync screen shows. The invite carries where the device answers and the one-time code that opens the pairing; the device is recorded waiting for the person's approval, which is what lets it read and write anything here. */
  pair_device(args: {
    invite: string;
  }): Promise<void>;
  /** Runs `pair_device` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  pair_deviceMany(items: PairDeviceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Approves a device that paired: Grayjay shows the same fingerprint on both ends. */
  approve_device(args: {
    device: PairedDevice;
  }): Promise<void>;
  /** Runs `approve_device` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  approve_deviceMany(items: ApproveDeviceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Revokes a device: its key can no longer read or write anything here. */
  revoke_device(args: {
    device: PairedDevice;
  }): Promise<void>;
  /** Runs `revoke_device` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  revoke_deviceMany(items: RevokeDeviceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Exchanges with every approved device now: reads the subscriptions and watch history Grayjay holds, and sends this client's subscriptions that way. */
  sync_devices(): Promise<void>;
  /** Runs `sync_devices` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  sync_devicesMany(items: SyncDevicesArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The approved devices are exchanged with on the timer, as Grayjay's own app does while it is open, so what either app adds reaches the other without anyone pressing. Nothing runs until a person has synced once by hand. */
  auto_sync(): Promise<void>;
  /** Runs `auto_sync` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  auto_syncMany(items: AutoSyncArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The Grayjay operation ended: a pairing keeps the device it made, an exchange what it brought, and a failure or a stop says so in the window. */
  grayjay_ended(args: {
    j: unknown;
  }): Promise<void>;
  /** Runs `grayjay_ended` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  grayjay_endedMany(items: GrayjayEndedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps the device a pairing made, waiting for the person's approval: the fingerprint beside it is what Grayjay shows on the other end. */
  keep_paired(args: {
    answer: string;
  }): Promise<void>;
  /** Runs `keep_paired` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  keep_pairedMany(items: KeepPairedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps what an exchange came to: how each device stands, the subscriptions the other apps hold, and the videos they watched. */
  keep_exchange(args: {
    answer: string;
  }): Promise<void>;
  /** Runs `keep_exchange` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  keep_exchangeMany(items: KeepExchangeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Searches YouTube: the service answers with channels, playlists and videos, written where the search view lists them. The answer arrives through the platform's network, never the app's. */
  search(args?: {
    query?: string;
  }): Promise<void>;
  /** Narrows the search to the kinds the person chose: `all`, `video`, `channel` or `playlist`. What the service found is kept, and the page shows the kind chosen alone, as the old chips did; asking again is the search field's, which carries the kind with it. */
  filter_search(args: {
    kind: string;
  }): Promise<void>;
  /** Runs `filter_search` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  filter_searchMany(items: FilterSearchArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Narrows the search to how long its videos run: `any`, `short`, `medium` or `long`. */
  filter_length(args: {
    length: string;
  }): Promise<void>;
  /** Narrows the search to how recent its results are: `any`, `hour`, `today`, `week`, `month` or `year`. */
  filter_published(args: {
    published: string;
  }): Promise<void>;
  /** Leaves the search the feed is showing for the place the person chose: the results stay where the service wrote them (found again by the query that found them), and the client shows the place chosen. */
  end_search(): Promise<void>;
  /** Runs `end_search` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  end_searchMany(items: EndSearchArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks the service for the newest videos of each subscribed channel: what the subscriptions feed shows. */
  refresh_feed(): Promise<void>;
  /** Asks the service for the videos it lists as trending. */
  load_trending(): Promise<void>;
  /** Opens a link of the scheme this client claims: the video (`tube:watch?v=<key>`), channel (`tube:channel?key=<key>`) or playlist (`tube:playlist?key=<key>`) it names, or the search it asks for (`tube:search?q=<query>`). A shell hands a link here whether or not a window of the client is open, and what this returns opens in the window. The host publishes it as the system's `open-link` on the client's root, which is what a shell hands a link to. */
  open_link(args: {
    link: string;
  }): Promise<unknown>;
  /** Likes a comment, or takes the like back: the person's own, kept with the workspace, since YouTube lets no app act as them. */
  like_comment(args: {
    c: Comment;
    liked?: boolean;
  }): Promise<void>;
  /** Runs `like_comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  like_commentMany(items: LikeCommentArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Plays the video a history entry names, from where the person left off. */
  play_again(args: {
    h: WatchEntry;
  }): Promise<void>;
}

/** The actions `YouTube` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const YOUTUBE_ACTIONS = ["play_queue", "session", "play_items", "play_itemsMany", "autoplay_next", "delete_history", "delete_historyMany", "set_autoplay", "set_autoplayMany", "set_quality", "set_qualityMany", "set_sponsorblock", "set_sponsorblockMany", "set_dearrow", "set_dearrowMany", "set_dislikes", "set_dislikesMany", "set_comments", "set_commentsMany", "set_speed", "set_speedMany", "pair_device", "pair_deviceMany", "approve_device", "approve_deviceMany", "revoke_device", "revoke_deviceMany", "sync_devices", "sync_devicesMany", "auto_sync", "auto_syncMany", "grayjay_ended", "grayjay_endedMany", "keep_paired", "keep_pairedMany", "keep_exchange", "keep_exchangeMany", "search", "filter_search", "filter_searchMany", "filter_length", "filter_published", "end_search", "end_searchMany", "refresh_feed", "load_trending", "open_link", "like_comment", "like_commentMany", "play_again"] as const;

/** The `tl.so.tilo.tube.YouTube` kind at `path`. */
export function youTube(ws: tilo.Workspace, path: string): YouTube {
  return resourceProxy(ws, path, SURFACE, YOUTUBE_ACTIONS) as unknown as YouTube;
}

/** YouTube — `tl.so.tilo.tube`. */
export interface Tube extends tilo.Resource {
}

/** The actions `Tube` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TUBE_ACTIONS = [] as const;

/** The `tl.so.tilo.tube` kind at `path`. */
export function tube(ws: tilo.Workspace, path: string): Tube {
  return resourceProxy(ws, path, SURFACE, TUBE_ACTIONS) as unknown as Tube;
}

