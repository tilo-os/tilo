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
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

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

/** Video Player — `video-player.library`. */
export interface Library extends tilo.Resource {
  playlists: Playlist[];
  items: PlaylistItem[];
  annotations: TimestampAnnotation[];
  readonly transports: Record<string, SharedTransport>;
  createPlaylist(args: {
    name: string;
  }): Promise<tilo.Run | void>;
  deletePlaylist(args: {
    playlist: string;
  }): Promise<tilo.Run | void>;
  addToPlaylist(args: {
    playlist: string;
    media_ref: string;
    title: string;
  }): Promise<tilo.Run | void>;
  addToWatchLater(args: {
    media_ref: string;
    title: string;
  }): Promise<tilo.Run | void>;
  addAnnotation(args: {
    media_ref: string;
    time_ms: number;
    text?: string;
  }): Promise<tilo.Run | void>;
  deleteAnnotation(args: {
    annotation: string;
  }): Promise<tilo.Run | void>;
  setTransport(args: {
    media_ref: string;
    playing: boolean;
    position_ms: number;
    rate: number;
    epoch_ms: number;
  }): Promise<tilo.Run | void>;
  openVideo(args?: {
    path?: string;
  }): Promise<void>;
  clearRecent(): Promise<void>;
}

/** The `video-player.library` kind at `path`. */
export function library(ws: tilo.Workspace, path: string): Library {
  return resourceProxy(ws, path) as unknown as Library;
}

/** Recent videos — `video-player.recent`. */
export interface Recent extends tilo.Resource {
  devices: Record<string, unknown>;
  keep(args: {
    device: string;
    rows: unknown;
  }): Promise<tilo.Run | void>;
  stood(args: {
    device: string;
    id: string;
    position_ms: number;
  }): Promise<tilo.Run | void>;
}

/** The `video-player.recent` kind at `path`. */
export function recent(ws: tilo.Workspace, path: string): Recent {
  return resourceProxy(ws, path) as unknown as Recent;
}

