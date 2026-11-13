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
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** The version of the surface these types were made from: a workspace whose app
 * publishes another refuses their actions rather than run what they no longer
 * describe. */
export const SURFACE = "so.tilo.tube@fdb44416bbb8c189";

export interface Reply {
  comment: string;
  text: string;
}

/** Tube — `tube.library`. */
export interface Library extends tilo.Resource {
  likes: string[];
  queue: string[];
  subscriptions: string[];
  comment_likes: string[];
  replies: Reply[];
  replyToComment(args: {
    comment: string;
    text: string;
  }): Promise<tilo.Run | void>;
  retract(args: {
    reply: string;
  }): Promise<tilo.Run | void>;
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

/** The `tube.library` kind at `path`. */
export function library(ws: tilo.Workspace, path: string): Library {
  return resourceProxy(ws, path, SURFACE) as unknown as Library;
}

/** Watch history — `tube.watch-history`. */
export interface WatchHistory extends tilo.Resource {
  watched: Record<string, number>;
  positions: Record<string, number>;
  record(args: {
    video: string;
    at_ms: number;
  }): Promise<tilo.Run | void>;
  progress(args: {
    video: string;
    position_ms: number;
    duration_ms: number;
  }): Promise<tilo.Run | void>;
  import(args: {
    watched: unknown[];
  }): Promise<tilo.Run | void>;
  forget(args: {
    video: string;
  }): Promise<tilo.Run | void>;
  clear(): Promise<tilo.Run | void>;
}

/** The `tube.watch-history` kind at `path`. */
export function watchHistory(ws: tilo.Workspace, path: string): WatchHistory {
  return resourceProxy(ws, path, SURFACE) as unknown as WatchHistory;
}

