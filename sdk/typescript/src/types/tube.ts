/**
 * Types for Tube (`so.tilo.tube`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Library` is a `Resource` of the app's `tube.library`, made by its factory with the
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
export const SURFACE = "so.tilo.tube@352bce080225ba43";

export interface Reply {
  comment: string;
  text: string;
}

/** The arguments of `reply-to-comment`. */
export interface ReplyToCommentArgs {
  comment: string;
  text: string;
}

/** The arguments of `retract`. */
export interface RetractArgs {
  reply: string;
}

/** The arguments of `record`. */
export interface RecordArgs {
  video: string;
  at_ms: number;
}

/** The arguments of `progress`. */
export interface ProgressArgs {
  video: string;
  position_ms: number;
  duration_ms: number;
}

/** The arguments of `import`. */
export interface ImportArgs {
  watched: unknown[];
}

/** The arguments of `forget`. */
export interface ForgetArgs {
  video: string;
}

/** The arguments of `clear`. */
export interface ClearArgs {
}

/** Tube — `tube.library`. */
export interface Library extends tilo.Resource {
  likes: string[];
  queue: string[];
  subscriptions: string[];
  comment_likes: string[];
  replies: Array<tilo.Entry<Reply>>;
  replyToComment(args: {
    comment: string;
    text: string;
  }): Promise<void>;
  /** Runs `reply-to-comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  replyToCommentMany(items: ReplyToCommentArgs[]): Promise<tilo.ActionAnswer[]>;
  retract(args: {
    reply: string;
  }): Promise<void>;
  /** Runs `retract` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  retractMany(items: RetractArgs[]): Promise<tilo.ActionAnswer[]>;
  openVideo(args: {
    video: string;
  }): Promise<void>;
  openCreator(args: {
    creator: string;
  }): Promise<void>;
  openPlaylist(args: {
    playlist: string;
  }): Promise<void>;
  search(args: {
    query: string;
  }): Promise<void>;
  togglePlayback(): Promise<void>;
  seek(args: {
    seconds: number;
  }): Promise<void>;
  skipSegment(): Promise<void>;
  share(args: {
    video: string;
  }): Promise<void>;
  markWatched(args: {
    video: string;
  }): Promise<void>;
  forgetWatched(args: {
    video: string;
  }): Promise<void>;
  clearHistory(): Promise<void>;
  setPreference(args: {
    preference: string;
    enabled: boolean;
  }): Promise<void>;
  setPreferredQuality(args: {
    quality: string;
  }): Promise<void>;
  setSegmentHandling(args: {
    category: string;
    mode: string;
  }): Promise<void>;
}

/** The actions `Library` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LIBRARY_ACTIONS = ["replyToComment", "replyToCommentMany", "retract", "retractMany", "openVideo", "openCreator", "openPlaylist", "search", "togglePlayback", "seek", "skipSegment", "share", "markWatched", "forgetWatched", "clearHistory", "setPreference", "setPreferredQuality", "setSegmentHandling"] as const;

/** The `tube.library` kind at `path`. */
export function library(ws: tilo.Workspace, path: string): Library {
  return resourceProxy(ws, path, SURFACE, LIBRARY_ACTIONS) as unknown as Library;
}

/** Watch history — `tube.watch-history`. */
export interface WatchHistory extends tilo.Resource {
  watched: Record<string, number>;
  positions: Record<string, number>;
  record(args: {
    video: string;
    at_ms: number;
  }): Promise<void>;
  /** Runs `record` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  recordMany(items: RecordArgs[]): Promise<tilo.ActionAnswer[]>;
  progress(args: {
    video: string;
    position_ms: number;
    duration_ms: number;
  }): Promise<void>;
  /** Runs `progress` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  progressMany(items: ProgressArgs[]): Promise<tilo.ActionAnswer[]>;
  import(args: {
    watched: unknown[];
  }): Promise<void>;
  /** Runs `import` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  importMany(items: ImportArgs[]): Promise<tilo.ActionAnswer[]>;
  forget(args: {
    video: string;
  }): Promise<void>;
  /** Runs `forget` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  forgetMany(items: ForgetArgs[]): Promise<tilo.ActionAnswer[]>;
  clear(): Promise<void>;
  /** Runs `clear` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clearMany(items: ClearArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `WatchHistory` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WATCHHISTORY_ACTIONS = ["record", "recordMany", "progress", "progressMany", "import", "importMany", "forget", "forgetMany", "clear", "clearMany"] as const;

/** The `tube.watch-history` kind at `path`. */
export function watchHistory(ws: tilo.Workspace, path: string): WatchHistory {
  return resourceProxy(ws, path, SURFACE, WATCHHISTORY_ACTIONS) as unknown as WatchHistory;
}

