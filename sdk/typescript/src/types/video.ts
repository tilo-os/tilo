/**
 * Types for Video (`so.tilo.video`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`video/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.video@28f43ffb04ea7f57";

/** The arguments of `toggle_playing`. */
export interface TogglePlayingArgs {
}

/** The arguments of `delete_playlist`. */
export interface DeletePlaylistArgs {
  pl: Playlist;
}

/** The arguments of `add_to_playlist`. */
export interface AddToPlaylistArgs {
  pl: Playlist;
  c: Clip;
}

/** The arguments of `add_to_watch_later`. */
export interface AddToWatchLaterArgs {
  c: Clip;
}

/** The arguments of `delete_bookmark`. */
export interface DeleteBookmarkArgs {
  b: Bookmark;
}

/** The arguments of `open_dropped`. */
export interface OpenDroppedArgs {
  files: unknown[];
}

/** The arguments of `play_clip`. */
export interface PlayClipArgs {
  c: Clip;
}

/** The arguments of `restart_clip`. */
export interface RestartClipArgs {
  c: Clip;
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

/** Clip — `tl.so.tilo.video.Clip`. */
export interface Clip extends tilo.Resource {
  sit6wbwo: string;  // title
  vnxskfep: string;  // source
  /** Plays or pauses the session when it is showing `c`: what the P key and the window's player control run, without needing the root passed in. */
  toggle_playing(): Promise<void>;
  /** Runs `toggle_playing` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  toggle_playingMany(items: TogglePlayingArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Clip` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CLIP_ACTIONS = ["toggle_playing", "toggle_playingMany"] as const;

/** The `tl.so.tilo.video.Clip` kind at `path`. */
export function clip(ws: tilo.Workspace, path: string): Clip {
  return resourceProxy(ws, path, SURFACE, CLIP_ACTIONS) as unknown as Clip;
}

/** Playlist — `tl.so.tilo.video.Playlist`. */
export interface Playlist extends tilo.Resource {
  hktiqra6: string;  // name
}

/** The actions `Playlist` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PLAYLIST_ACTIONS = [] as const;

/** The `tl.so.tilo.video.Playlist` kind at `path`. */
export function playlist(ws: tilo.Workspace, path: string): Playlist {
  return resourceProxy(ws, path, SURFACE, PLAYLIST_ACTIONS) as unknown as Playlist;
}

/** PlaylistEntry — `tl.so.tilo.video.PlaylistEntry`. */
export interface PlaylistEntry extends tilo.Resource {
  pmcgj5bo: Playlist;  // playlist
  gofxf7oh: Clip;  // clip
}

/** The actions `PlaylistEntry` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PLAYLISTENTRY_ACTIONS = [] as const;

/** The `tl.so.tilo.video.PlaylistEntry` kind at `path`. */
export function playlistEntry(ws: tilo.Workspace, path: string): PlaylistEntry {
  return resourceProxy(ws, path, SURFACE, PLAYLISTENTRY_ACTIONS) as unknown as PlaylistEntry;
}

/** Bookmark — `tl.so.tilo.video.Bookmark`. */
export interface Bookmark extends tilo.Resource {
  h4e7iq7t: Clip;  // clip
  cwamirwi: number;  // at
  n323noym: string;  // text
}

/** The actions `Bookmark` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const BOOKMARK_ACTIONS = [] as const;

/** The `tl.so.tilo.video.Bookmark` kind at `path`. */
export function bookmark(ws: tilo.Workspace, path: string): Bookmark {
  return resourceProxy(ws, path, SURFACE, BOOKMARK_ACTIONS) as unknown as Bookmark;
}

/** Library — `tl.so.tilo.video.Library`. */
export interface Library extends tilo.Resource {
  cfgqumjg: Array<tilo.Entry<Clip>>;  // clips
  "7sp4sk7x": Array<tilo.Entry<Playlist>>;  // playlists
  ve6kofwh: Array<tilo.Entry<PlaylistEntry>>;  // entries
  srzszayp: Array<tilo.Entry<Clip>>;  // watch_later
  "265jfqy5": Array<tilo.Entry<Bookmark>>;  // bookmarks
  jcuq2e4s: unknown | null;  // playing
  eajmuh6i: Array<tilo.Entry<unknown>>;  // sessions
  /** Opens a video file: the file a person picked, or one Files handed over, and plays it where it was left. The clip it already made opens instead of a second copy, so its place in history stays one. */
  open_video(args: {
    file: unknown;
  }): Promise<Clip>;
  /** Starts an empty playlist. */
  create_playlist(args: {
    name: string;
  }): Promise<Playlist>;
  /** Deletes a playlist and the entries that name it. */
  delete_playlist(args: {
    pl: Playlist;
  }): Promise<void>;
  /** Runs `delete_playlist` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_playlistMany(items: DeletePlaylistArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds `c` to `pl`, once. */
  add_to_playlist(args: {
    pl: Playlist;
    c: Clip;
  }): Promise<void>;
  /** Runs `add_to_playlist` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  add_to_playlistMany(items: AddToPlaylistArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps `c` to watch later, once. */
  add_to_watch_later(args: {
    c: Clip;
  }): Promise<void>;
  /** Runs `add_to_watch_later` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  add_to_watch_laterMany(items: AddToWatchLaterArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Bookmarks the moment `at` of `c`, with `text` when the person wrote something. */
  add_bookmark(args: {
    c: Clip;
    at: number;
    text?: string;
  }): Promise<Bookmark>;
  /** Deletes a bookmark or note. */
  delete_bookmark(args: {
    b: Bookmark;
  }): Promise<void>;
  /** Runs `delete_bookmark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_bookmarkMany(items: DeleteBookmarkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens the videos a person dropped on the window: each file becomes its clip, and the first plays in the shared session. */
  open_dropped(args: {
    files: unknown[];
  }): Promise<void>;
  /** Runs `open_dropped` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  open_droppedMany(items: OpenDroppedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The session this app plays through: the one it has, or one made now. It is kept both in `playing`, which a view reads, and in `sessions`, so the record is a resource of its own. */
  session(): Promise<unknown>;
  /** Plays `c` in the shared session, from where it was left (the beginning the first time): what the window's Play button, the palette and an agent's `play` run. The place is the shared session's own record, so the video continues on this device or another. */
  play_clip(args: {
    c: Clip;
  }): Promise<void>;
  /** Runs `play_clip` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  play_clipMany(items: PlayClipArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Plays `c` from its beginning, as a person who watched it before chooses. */
  restart_clip(args: {
    c: Clip;
  }): Promise<void>;
  /** Runs `restart_clip` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  restart_clipMany(items: RestartClipArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Library` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LIBRARY_ACTIONS = ["open_video", "create_playlist", "delete_playlist", "delete_playlistMany", "add_to_playlist", "add_to_playlistMany", "add_to_watch_later", "add_to_watch_laterMany", "add_bookmark", "delete_bookmark", "delete_bookmarkMany", "open_dropped", "open_droppedMany", "session", "play_clip", "play_clipMany", "restart_clip", "restart_clipMany"] as const;

/** The `tl.so.tilo.video.Library` kind at `path`. */
export function library(ws: tilo.Workspace, path: string): Library {
  return resourceProxy(ws, path, SURFACE, LIBRARY_ACTIONS) as unknown as Library;
}

/** Video — `tl.so.tilo.video`. */
export interface Video extends tilo.Resource {
}

/** The actions `Video` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const VIDEO_ACTIONS = [] as const;

/** The `tl.so.tilo.video` kind at `path`. */
export function video(ws: tilo.Workspace, path: string): Video {
  return resourceProxy(ws, path, SURFACE, VIDEO_ACTIONS) as unknown as Video;
}

