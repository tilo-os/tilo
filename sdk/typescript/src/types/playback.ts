/**
 * Types for Playback (`so.tilo.playback`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`playback/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.playback@5c5a552fa70424c3";

/** Whether the player stands still, plays, or has nothing at all. One of "stopped", "playing", "paused". */
export type PlaybackState = "stopped" | "playing" | "paused";

/** The arguments of `play_list`. */
export interface PlayListArgs {
  items: Item[];
  at?: number;
}

/** The arguments of `set_current`. */
export interface SetCurrentArgs {
  item: Item;
}

/** The arguments of `set_playing`. */
export interface SetPlayingArgs {
  on: boolean;
}

/** The arguments of `set_source`. */
export interface SetSourceArgs {
  source: string;
}

/** The arguments of `set_length`. */
export interface SetLengthArgs {
  seconds: number;
}

/** The arguments of `remember_position`. */
export interface RememberPositionArgs {
  seconds: number;
  when: string;
}

/** The arguments of `set_position`. */
export interface SetPositionArgs {
  seconds: number;
}

/** The arguments of `set_share`. */
export interface SetShareArgs {
  share: number;
}

/** The arguments of `resume`. */
export interface ResumeArgs {
  ref: string;
}

/** The arguments of `play`. */
export interface PlayArgs {
}

/** The arguments of `toggle`. */
export interface ToggleArgs {
}

/** The arguments of `pause`. */
export interface PauseArgs {
}

/** The arguments of `stop`. */
export interface StopArgs {
}

/** The arguments of `next`. */
export interface NextArgs {
}

/** The arguments of `previous`. */
export interface PreviousArgs {
}

/** The arguments of `select`. */
export interface SelectArgs {
  at: number;
}

/** The arguments of `seek_by`. */
export interface SeekByArgs {
  seconds?: number;
}

/** The arguments of `seek`. */
export interface SeekArgs {
  place?: number;
}

/** The arguments of `set_volume`. */
export interface SetVolumeArgs {
  level?: number;
}

/** The arguments of `mute`. */
export interface MuteArgs {
}

/** The arguments of `shuffle_toggle`. */
export interface ShuffleToggleArgs {
  shuffle?: boolean;
}

/** The arguments of `cycle_repeat`. */
export interface CycleRepeatArgs {
}

/** The arguments of `queue_add`. */
export interface QueueAddArgs {
  item: Item;
}

/** The arguments of `queue_play_next`. */
export interface QueuePlayNextArgs {
  item: Item;
}

/** The arguments of `queue_remove`. */
export interface QueueRemoveArgs {
  index: number;
  queue_at?: number;
}

/** The arguments of `queue_move`. */
export interface QueueMoveArgs {
  from: string;
  to: string;
}

/** The arguments of `add_output`. */
export interface AddOutputArgs {
  output: Output;
}

/** The arguments of `remove_output`. */
export interface RemoveOutputArgs {
  index: number;
}

/** The arguments of `move_to`. */
export interface MoveToArgs {
  from: number;
  to: number;
}

/** The arguments of `set_output_volume`. */
export interface SetOutputVolumeArgs {
  index: number;
  level?: number;
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

/** Output — `tl.so.tilo.playback.Output`. */
export interface Output extends tilo.Resource {
  mtgbfbe3: string;  // kind
  q2afggjg: string;  // device
  duy3y22z: string;  // name
  sz4b5nfh: number;  // volume
  ewrhlhvs: boolean;  // synced
}

/** The actions `Output` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OUTPUT_ACTIONS = [] as const;

/** The `tl.so.tilo.playback.Output` kind at `path`. */
export function output(ws: tilo.Workspace, path: string): Output {
  return resourceProxy(ws, path, SURFACE, OUTPUT_ACTIONS) as unknown as Output;
}

/** Item — `tl.so.tilo.playback.Item`. */
export interface Item extends tilo.Resource {
  jsytyrgy: string;  // ref
  hbrdxjws: string;  // title
  "3sty6xvi": string;  // artist
  wzxeznnc: string;  // cover
  iueovkzr: string;  // source
  ggr7nnxj: number;  // length
}

/** The actions `Item` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ITEM_ACTIONS = [] as const;

/** The `tl.so.tilo.playback.Item` kind at `path`. */
export function item(ws: tilo.Workspace, path: string): Item {
  return resourceProxy(ws, path, SURFACE, ITEM_ACTIONS) as unknown as Item;
}

/** Progress — `tl.so.tilo.playback.Progress`. */
export interface Progress extends tilo.Resource {
  ijcc2upy: string;  // ref
  owteykf6: number;  // position
  q64zycak: string;  // updated
}

/** The actions `Progress` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PROGRESS_ACTIONS = [] as const;

/** The `tl.so.tilo.playback.Progress` kind at `path`. */
export function progress(ws: tilo.Workspace, path: string): Progress {
  return resourceProxy(ws, path, SURFACE, PROGRESS_ACTIONS) as unknown as Progress;
}

/** Playback — `tl.so.tilo.playback.Playback`. */
export interface Playback extends tilo.Resource {
  hnwowp42: string;  // track
  sx7vtcfl: string;  // title
  eejl5scp: string;  // artist
  rjbz4skr: string;  // cover
  tafuqjic: string;  // source
  xcyilpu4: PlaybackState;  // state
  p2dmm6gi: Array<tilo.Entry<Output>>;  // outputs
  pkexjxh3: number;  // at
  min4lfee: number;  // length
  udayujnj: number;  // volume
  "4t7z5bco": boolean;  // shuffle
  "2l7pa2rs": string;  // repeat
  wwdrlrqy: Array<tilo.Entry<Item>>;  // queue
  krqh6s6b: number;  // queue_at
  eixby7cm: Array<tilo.Entry<Progress>>;  // progress
  /** Plays the items `items` names, in the order given, from the one at `at`: the queue is what follows, and the item at `at` plays. The app that calls this fills the title, artist, cover, source and length it resolves from its own record; the shared actions below move the queue without reading another app's records. */
  play_list(args: {
    items: Item[];
    at?: number;
  }): Promise<void>;
  /** Runs `play_list` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  play_listMany(items: PlayListArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The session shows `item`: its fields are the item's, so a shared next or previous never leaves the title, cover or source of the one before it. */
  set_current(args: {
    item: Item;
  }): Promise<void>;
  /** Runs `set_current` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_currentMany(items: SetCurrentArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Plays or pauses what plays, as a player's own control and a media key ask it. */
  set_playing(args: {
    on: boolean;
  }): Promise<void>;
  /** Runs `set_playing` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_playingMany(items: SetPlayingArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Where what plays comes from, as the app that knows it answered again: a video's other quality, or the address it was asked for once more. The player opens it, and the live card and another device name the same bytes. */
  set_source(args: {
    source: string;
  }): Promise<void>;
  /** Runs `set_source` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_sourceMany(items: SetSourceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** What the player reports of its length, once the decoder knows it. */
  set_length(args: {
    seconds: number;
  }): Promise<void>;
  /** Runs `set_length` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_lengthMany(items: SetLengthArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps the session's place and the item's as `when` records them. It has no effect of its own, so an app's action that plays can call it inline: an action that needs a moment is a step of its own, which a nested call only suggests. */
  remember_position(args: {
    seconds: number;
    when: string;
  }): Promise<void>;
  /** Runs `remember_position` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remember_positionMany(items: RememberPositionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Where the player stands, as it reports it, so a later opening resumes there: the session's place and the item's, kept so another device or a later opening continues from it. */
  set_position(args: {
    seconds: number;
  }): Promise<void>;
  /** Runs `set_position` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_positionMany(items: SetPositionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Where the player stands, as the share of its length a player reports (`0..=1`), kept so a later opening resumes there. */
  set_share(args: {
    share: number;
  }): Promise<void>;
  /** Runs `set_share` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_shareMany(items: SetShareArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves what plays to where `ref` was left, so a video or a song continues where the person stopped instead of beginning again. */
  resume(args: {
    ref: string;
  }): Promise<void>;
  /** Runs `resume` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  resumeMany(items: ResumeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Plays what is chosen, or begins again where it stands. */
  play(): Promise<void>;
  /** Runs `play` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  playMany(items: PlayArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Plays or pauses what plays. */
  toggle(): Promise<void>;
  /** Runs `toggle` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  toggleMany(items: ToggleArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Pauses what plays and starts it again where it stood ([`toggle`]'s own job). */
  pause(): Promise<void>;
  /** Runs `pause` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  pauseMany(items: PauseArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Stops what plays: it plays on no device, and the shell shows it no more. */
  stop(): Promise<void>;
  /** Runs `stop` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  stopMany(items: StopArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The item after this one: the queue's next, or the first when repeat is on. */
  next(): Promise<void>;
  /** Runs `next` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  nextMany(items: NextArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The item before this one. */
  previous(): Promise<void>;
  /** Runs `previous` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  previousMany(items: PreviousArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Plays the queued item at `at`, keeping the queue as it stands. */
  select(args: {
    at: number;
  }): Promise<void>;
  /** Runs `select` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  selectMany(items: SelectArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves what plays by `seconds`, for the live card's two seek controls. */
  seek_by(args?: {
    seconds?: number;
  }): Promise<void>;
  /** Runs `seek_by` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  seek_byMany(items: SeekByArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves what plays to `place`, a fraction of its length, and keeps the place so a later opening continues there. */
  seek(args?: {
    place?: number;
  }): Promise<void>;
  /** Runs `seek` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  seekMany(items: SeekArgs[]): Promise<tilo.ActionAnswer[]>;
  /** How loud the session is. */
  set_volume(args?: {
    level?: number;
  }): Promise<void>;
  /** Runs `set_volume` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_volumeMany(items: SetVolumeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Silences the session, or brings back the volume it had. */
  mute(): Promise<void>;
  /** Runs `mute` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  muteMany(items: MuteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shuffles the queue, or puts its order back. The item that plays stays first. */
  shuffle_toggle(args?: {
    shuffle?: boolean;
  }): Promise<void>;
  /** Runs `shuffle_toggle` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  shuffle_toggleMany(items: ShuffleToggleArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves repeat on: off, all, one. */
  cycle_repeat(): Promise<void>;
  /** Runs `cycle_repeat` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  cycle_repeatMany(items: CycleRepeatArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds `item` after what plays. */
  queue_add(args: {
    item: Item;
  }): Promise<void>;
  /** Runs `queue_add` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  queue_addMany(items: QueueAddArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds `item` right after what plays. */
  queue_play_next(args: {
    item: Item;
  }): Promise<void>;
  /** Runs `queue_play_next` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  queue_play_nextMany(items: QueuePlayNextArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes the item at `index` out of the queue. */
  queue_remove(args: {
    index: number;
    queue_at?: number;
  }): Promise<void>;
  /** Runs `queue_remove` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  queue_removeMany(items: QueueRemoveArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves the item called `from` to stand where `to` stands: the queue row a person dragged, and the ids its reorder reports. */
  queue_move(args: {
    from: string;
    to: string;
  }): Promise<void>;
  /** Runs `queue_move` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  queue_moveMany(items: QueueMoveArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts the sound on one more place: a speaker in a room, or a cast target. */
  add_output(args: {
    output: Output;
  }): Promise<void>;
  /** Runs `add_output` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  add_outputMany(items: AddOutputArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes the place at `index` out of the session; this device is never taken out. */
  remove_output(args: {
    index: number;
  }): Promise<void>;
  /** Runs `remove_output` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_outputMany(items: RemoveOutputArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves the place at `from` to where `to` is: 'continue on the speaker' is moving the session from this device to it. */
  move_to(args: {
    from: number;
    to: number;
  }): Promise<void>;
  /** Runs `move_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_toMany(items: MoveToArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets how loud the place at `index` is. */
  set_output_volume(args: {
    index: number;
    level?: number;
  }): Promise<void>;
  /** Runs `set_output_volume` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_output_volumeMany(items: SetOutputVolumeArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Playback` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PLAYBACK_ACTIONS = ["play_list", "play_listMany", "set_current", "set_currentMany", "set_playing", "set_playingMany", "set_source", "set_sourceMany", "set_length", "set_lengthMany", "remember_position", "remember_positionMany", "set_position", "set_positionMany", "set_share", "set_shareMany", "resume", "resumeMany", "play", "playMany", "toggle", "toggleMany", "pause", "pauseMany", "stop", "stopMany", "next", "nextMany", "previous", "previousMany", "select", "selectMany", "seek_by", "seek_byMany", "seek", "seekMany", "set_volume", "set_volumeMany", "mute", "muteMany", "shuffle_toggle", "shuffle_toggleMany", "cycle_repeat", "cycle_repeatMany", "queue_add", "queue_addMany", "queue_play_next", "queue_play_nextMany", "queue_remove", "queue_removeMany", "queue_move", "queue_moveMany", "add_output", "add_outputMany", "remove_output", "remove_outputMany", "move_to", "move_toMany", "set_output_volume", "set_output_volumeMany"] as const;

/** The `tl.so.tilo.playback.Playback` kind at `path`. */
export function playback(ws: tilo.Workspace, path: string): Playback {
  return resourceProxy(ws, path, SURFACE, PLAYBACK_ACTIONS) as unknown as Playback;
}

/** Playback — `tl.so.tilo.playback`. */
export interface TiloPlayback extends tilo.Resource {
}

/** The actions `TiloPlayback` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TILOPLAYBACK_ACTIONS = [] as const;

/** The `tl.so.tilo.playback` kind at `path`. */
export function tiloPlayback(ws: tilo.Workspace, path: string): TiloPlayback {
  return resourceProxy(ws, path, SURFACE, TILOPLAYBACK_ACTIONS) as unknown as TiloPlayback;
}

