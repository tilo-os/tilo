/**
 * Types for Video Player (`so.tilo.video-player`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Library` is a `Resource` of the app's `video-player.library`, made by its factory with the
 * resource's path (`video-player/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.video-player@1932567c11289a24";

export interface Playlist {
  name: string;
}

export interface PlaylistItem {
  playlist: string;
  media_ref: string;
  title: string;
}

export interface SharedTransport {
  playing: boolean;
  position_ms: number;
  rate: number;
  epoch_ms: number;
}

export interface TimestampAnnotation {
  media_ref: string;
  time_ms: number;
  text: string;
}

/** The arguments of `create-playlist`. */
export interface CreatePlaylistArgs {
  name: string;
}

/** The arguments of `delete-playlist`. */
export interface DeletePlaylistArgs {
  playlist: string;
}

/** The arguments of `add-to-playlist`. */
export interface AddToPlaylistArgs {
  playlist: string;
  media_ref: string;
  title: string;
}

/** The arguments of `add-to-watch-later`. */
export interface AddToWatchLaterArgs {
  media_ref: string;
  title: string;
}

/** The arguments of `add-annotation`. */
export interface AddAnnotationArgs {
  media_ref: string;
  time_ms: number;
  text?: string;
}

/** The arguments of `delete-annotation`. */
export interface DeleteAnnotationArgs {
  annotation: string;
}

/** The arguments of `set-transport`. */
export interface SetTransportArgs {
  media_ref: string;
  playing: boolean;
  position_ms: number;
  rate: number;
  epoch_ms: number;
}

/** The arguments of `keep`. */
export interface KeepArgs {
  device: string;
  rows: unknown;
}

/** The arguments of `stood`. */
export interface StoodArgs {
  device: string;
  id: string;
  position_ms: number;
}

/** Video Player — `video-player.library`. */
export interface Library extends tilo.Resource {
  playlists: Array<tilo.Entry<Playlist>>;
  items: Array<tilo.Entry<PlaylistItem>>;
  annotations: Array<tilo.Entry<TimestampAnnotation>>;
  readonly transports: Record<string, SharedTransport>;
  createPlaylist(args: {
    name: string;
  }): Promise<void>;
  /** Runs `create-playlist` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  createPlaylistMany(items: CreatePlaylistArgs[]): Promise<tilo.ActionAnswer[]>;
  deletePlaylist(args: {
    playlist: string;
  }): Promise<void>;
  /** Runs `delete-playlist` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deletePlaylistMany(items: DeletePlaylistArgs[]): Promise<tilo.ActionAnswer[]>;
  addToPlaylist(args: {
    playlist: string;
    media_ref: string;
    title: string;
  }): Promise<void>;
  /** Runs `add-to-playlist` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  addToPlaylistMany(items: AddToPlaylistArgs[]): Promise<tilo.ActionAnswer[]>;
  addToWatchLater(args: {
    media_ref: string;
    title: string;
  }): Promise<void>;
  /** Runs `add-to-watch-later` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  addToWatchLaterMany(items: AddToWatchLaterArgs[]): Promise<tilo.ActionAnswer[]>;
  addAnnotation(args: {
    media_ref: string;
    time_ms: number;
    text?: string;
  }): Promise<void>;
  /** Runs `add-annotation` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  addAnnotationMany(items: AddAnnotationArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteAnnotation(args: {
    annotation: string;
  }): Promise<void>;
  /** Runs `delete-annotation` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteAnnotationMany(items: DeleteAnnotationArgs[]): Promise<tilo.ActionAnswer[]>;
  setTransport(args: {
    media_ref: string;
    playing: boolean;
    position_ms: number;
    rate: number;
    epoch_ms: number;
  }): Promise<void>;
  /** Runs `set-transport` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setTransportMany(items: SetTransportArgs[]): Promise<tilo.ActionAnswer[]>;
  openVideo(args?: {
    path?: string;
  }): Promise<void>;
  clearRecent(): Promise<void>;
}

/** The actions `Library` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LIBRARY_ACTIONS = ["createPlaylist", "createPlaylistMany", "deletePlaylist", "deletePlaylistMany", "addToPlaylist", "addToPlaylistMany", "addToWatchLater", "addToWatchLaterMany", "addAnnotation", "addAnnotationMany", "deleteAnnotation", "deleteAnnotationMany", "setTransport", "setTransportMany", "openVideo", "clearRecent"] as const;

/** The `video-player.library` kind at `path`. */
export function library(ws: tilo.Workspace, path: string): Library {
  return resourceProxy(ws, path, SURFACE, LIBRARY_ACTIONS) as unknown as Library;
}

/** Recent videos — `video-player.recent`. */
export interface Recent extends tilo.Resource {
  devices: Record<string, unknown>;
  keep(args: {
    device: string;
    rows: unknown;
  }): Promise<void>;
  /** Runs `keep` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  keepMany(items: KeepArgs[]): Promise<tilo.ActionAnswer[]>;
  stood(args: {
    device: string;
    id: string;
    position_ms: number;
  }): Promise<void>;
  /** Runs `stood` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  stoodMany(items: StoodArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Recent` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RECENT_ACTIONS = ["keep", "keepMany", "stood", "stoodMany"] as const;

/** The `video-player.recent` kind at `path`. */
export function recent(ws: tilo.Workspace, path: string): Recent {
  return resourceProxy(ws, path, SURFACE, RECENT_ACTIONS) as unknown as Recent;
}

