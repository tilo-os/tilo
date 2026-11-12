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
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "@tilo/sdk";
import type * as tilo from "@tilo/sdk";

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

export interface CatalogueTrack {
  track: Track;
  album: Album;
  artist: Artist;
}

export interface Playlist {
  id: string;
  name: string;
  description: string;
  tracks: string[];
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

/** Music — `music.library`. */
export interface Library extends tilo.Resource {
  recommendations: Preferences;
  readonly artists: Record<string, Artist>;
  readonly albums: Record<string, Album>;
  readonly tracks: Record<string, Track>;
  playlists: Playlist[];
  liked: string[];
  saved_albums: string[];
  followed_artists: string[];
  createPlaylist(args: {
    name: string;
    track?: string;
  }): Promise<tilo.Run | void>;
  deletePlaylist(args: {
    playlist: string;
  }): Promise<tilo.Run | void>;
  importTrack(args: {
    song: CatalogueTrack;
  }): Promise<tilo.Run | void>;
  spotifyAccountStatus(): Promise<void>;
  search(args: {
    query: string;
  }): Promise<void>;
}

/** The `music.library` kind at `path`. */
export function library(ws: tilo.Workspace, path: string): Library {
  return resourceProxy(ws, path) as unknown as Library;
}

