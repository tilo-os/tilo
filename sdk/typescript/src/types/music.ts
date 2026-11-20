/**
 * Types for Music (`so.tilo.music`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Library` is a `Resource` of the app's `music.library`, made by its factory with the
 * resource's path (`music/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.music@26a9509bd0593501";

export interface Album {
  id: string;
  title: string;
  artist_id: string;
  year: number;
}

export interface Artist {
  id: string;
  name: string;
  genre: string;
  monthly_listeners: number;
}

/** One of "unset", "disabled", "you_tube", "spotify". */
export type Catalogue = "unset" | "disabled" | "you_tube" | "spotify";

export interface CatalogueTrack {
  track: Track;
  album: Album;
  artist: Artist;
}

export interface Playlist {
  id: string;
  name: string;
  description: string;
  tracks: Array<tilo.Entry<string>>;
  cover: string;
}

export interface Preferences {
  likes: boolean;
  saved_albums: boolean;
  followed_artists: boolean;
  preferred_artists: string[];
  dismissed: string[];
}

export interface Track {
  id: string;
  title: string;
  album_id: string;
  artist_id: string;
  track_no: number;
  duration_s: number;
  yt_id: string;
  plays: number;
}

/** The arguments of `set-catalogue`. */
export interface SetCatalogueArgs {
  catalogue: Catalogue;
}

/** The arguments of `import-catalogue`. */
export interface ImportCatalogueArgs {
  catalogue: Catalogue;
}

/** The arguments of `create-playlist`. */
export interface CreatePlaylistArgs {
  name: string;
  track?: string;
}

/** The arguments of `delete-playlist`. */
export interface DeletePlaylistArgs {
  playlist: string;
}

/** The arguments of `import-track`. */
export interface ImportTrackArgs {
  song: CatalogueTrack;
}

/** Music — `music.library`. */
export interface Library extends tilo.Resource {
  readonly catalogue: Catalogue;
  recommendations: Preferences;
  readonly artists: Record<string, Artist>;
  readonly albums: Record<string, Album>;
  readonly tracks: Record<string, Track>;
  playlists: Array<tilo.Entry<Playlist>>;
  liked: string[];
  saved_albums: string[];
  followed_artists: string[];
  setCatalogue(args: {
    catalogue: Catalogue;
  }): Promise<void>;
  /** Runs `set-catalogue` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setCatalogueMany(items: SetCatalogueArgs[]): Promise<tilo.ActionAnswer[]>;
  importCatalogue(args: {
    catalogue: Catalogue;
  }): Promise<void>;
  /** Runs `import-catalogue` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  importCatalogueMany(items: ImportCatalogueArgs[]): Promise<tilo.ActionAnswer[]>;
  createPlaylist(args: {
    name: string;
    track?: string;
  }): Promise<void>;
  /** Runs `create-playlist` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  createPlaylistMany(items: CreatePlaylistArgs[]): Promise<tilo.ActionAnswer[]>;
  deletePlaylist(args: {
    playlist: string;
  }): Promise<void>;
  /** Runs `delete-playlist` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deletePlaylistMany(items: DeletePlaylistArgs[]): Promise<tilo.ActionAnswer[]>;
  importTrack(args: {
    song: CatalogueTrack;
  }): Promise<void>;
  /** Runs `import-track` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  importTrackMany(items: ImportTrackArgs[]): Promise<tilo.ActionAnswer[]>;
  spotifyAccountStatus(): Promise<void>;
  search(args: {
    query: string;
  }): Promise<void>;
}

/** The actions `Library` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LIBRARY_ACTIONS = ["setCatalogue", "setCatalogueMany", "importCatalogue", "importCatalogueMany", "createPlaylist", "createPlaylistMany", "deletePlaylist", "deletePlaylistMany", "importTrack", "importTrackMany", "spotifyAccountStatus", "search"] as const;

/** The `music.library` kind at `path`. */
export function library(ws: tilo.Workspace, path: string): Library {
  return resourceProxy(ws, path, SURFACE, LIBRARY_ACTIONS) as unknown as Library;
}

