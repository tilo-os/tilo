/**
 * Types for YouTube (`so.tilo.tube`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Channel` is a `Resource` of the app's `tl.so.tilo.tube.Channel`, made by its factory with the
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
export const SURFACE = "so.tilo.tube@5738a7401a7c766a";

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

/** The arguments of `delete_history`. */
export interface DeleteHistoryArgs {
}

/** The arguments of `set_autoplay`. */
export interface SetAutoplayArgs {
  on: boolean;
}

/** The arguments of `set_speed`. */
export interface SetSpeedArgs {
  speed: number;
}

/** The arguments of `set_quality`. */
export interface SetQualityArgs {
  quality: string;
}

/** The arguments of `set_captions`. */
export interface SetCaptionsArgs {
  on: boolean;
}

/** The arguments of `end_search`. */
export interface EndSearchArgs {
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
  l25oorez: string;  // thumbnail
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
  /** Plays the video, whether or not it was played before. */
  play(): Promise<void>;
  /** Plays the video from where the person left off, when they did not finish it. */
  resume(): Promise<void>;
  /** Records how far into the video the person is, and that they watched it now: it moves to the front of the history, and asks the service for the address its bytes play from when that is not known yet. Starting a video, resuming it and moving the playhead as it plays are one action, so a video has one place it is played from. */
  progress(args: {
    at: number;
  }): Promise<void>;
  /** Takes one video out of the person's own history. */
  forget(): Promise<void>;
  /** Runs `forget` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  forgetMany(items: ForgetArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks the service for where the video's bytes play from again, after the player could not show them: the address the client holds is the service's alone to write, and the answer it gives now is what the player opens. */
  retry_playback(): Promise<void>;
}

/** The actions `Video` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const VIDEO_ACTIONS = ["like", "likeMany", "watch_later", "watch_laterMany", "play", "resume", "progress", "forget", "forgetMany", "retry_playback"] as const;

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
  /** Plays the playlist from its first video. */
  play_playlist(): Promise<void>;
}

/** The actions `Playlist` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PLAYLIST_ACTIONS = ["refresh_playlist", "play_playlist"] as const;

/** The `tl.so.tilo.tube.Playlist` kind at `path`. */
export function playlist(ws: tilo.Workspace, path: string): Playlist {
  return resourceProxy(ws, path, SURFACE, PLAYLIST_ACTIONS) as unknown as Playlist;
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
  "5bpxm33f": number;  // speed
  lesyml4z: string;  // quality
  u3fgpv6e: boolean;  // captions
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
}

/** The actions `WatchEntry` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WATCHENTRY_ACTIONS = [] as const;

/** The `tl.so.tilo.tube.WatchEntry` kind at `path`. */
export function watchEntry(ws: tilo.Workspace, path: string): WatchEntry {
  return resourceProxy(ws, path, SURFACE, WATCHENTRY_ACTIONS) as unknown as WatchEntry;
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
  epgpppzy: Array<tilo.Entry<PlaybackPrefs>>;  // playback
  bqesicxq: string;  // search_text
  hcmy6dp7: string;  // search_state
  zjw3anaz: string;  // search_trouble
  nnvco6w3: string;  // feed_state
  r3ksw6bn: string;  // trouble
  qv7t2mza: boolean;  // online
  /** Plays the queue from its oldest video: what was put on it first plays first. */
  play_queue(): Promise<void>;
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
  /** How fast videos play for the person: 0.25 to 3, 1 for the usual speed. */
  set_speed(args: {
    speed: number;
  }): Promise<void>;
  /** Runs `set_speed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_speedMany(items: SetSpeedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The tallest picture the person wants, as YouTube names it; `auto` takes the best that fits. */
  set_quality(args: {
    quality: string;
  }): Promise<void>;
  /** Runs `set_quality` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_qualityMany(items: SetQualityArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Whether videos show their captions for the person. */
  set_captions(args: {
    on: boolean;
  }): Promise<void>;
  /** Runs `set_captions` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_captionsMany(items: SetCaptionsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Searches YouTube: the service answers with channels, playlists and videos, written where the search view lists them. The answer arrives through the platform's network, never the app's. */
  search(args?: {
    query?: string;
  }): Promise<void>;
  /** Leaves the search the feed is showing for the place the person chose: the results stay where the service wrote them (found again by the query that found them), and the client shows the place chosen. */
  end_search(): Promise<void>;
  /** Runs `end_search` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  end_searchMany(items: EndSearchArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks the service for the newest videos of each subscribed channel: what the subscriptions feed shows. */
  refresh_feed(): Promise<void>;
  /** Asks the service for the videos it lists as trending. */
  load_trending(): Promise<void>;
  /** Plays the video a history entry names, from where the person left off. */
  play_again(args: {
    h: WatchEntry;
  }): Promise<void>;
}

/** The actions `YouTube` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const YOUTUBE_ACTIONS = ["play_queue", "delete_history", "delete_historyMany", "set_autoplay", "set_autoplayMany", "set_speed", "set_speedMany", "set_quality", "set_qualityMany", "set_captions", "set_captionsMany", "search", "end_search", "end_searchMany", "refresh_feed", "load_trending", "play_again"] as const;

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

