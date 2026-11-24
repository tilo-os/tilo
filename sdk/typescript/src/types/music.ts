/**
 * Types for Music (`so.tilo.music`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
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
export const SURFACE = "so.tilo.music@c11132d5bc5b3e7b";

/** The arguments of `like`. */
export interface LikeArgs {
  liked?: boolean;
}

/** The arguments of `remove_offline`. */
export interface RemoveOfflineArgs {
}

/** The arguments of `play_album`. */
export interface PlayAlbumArgs {
}

/** The arguments of `save_album`. */
export interface SaveAlbumArgs {
  keep?: boolean;
}

/** The arguments of `follow_artist`. */
export interface FollowArtistArgs {
  followed?: boolean;
}

/** The arguments of `play_mixtape`. */
export interface PlayMixtapeArgs {
}

/** The arguments of `mixtape_add`. */
export interface MixtapeAddArgs {
  t: Track;
}

/** The arguments of `mixtape_remove`. */
export interface MixtapeRemoveArgs {
  t: Track;
}

/** The arguments of `pick_cover`. */
export interface PickCoverArgs {
  file: unknown;
}

/** The arguments of `set_cover`. */
export interface SetCoverArgs {
  file: unknown;
}

/** The arguments of `add_to_queue`. */
export interface AddToQueueArgs {
  t: Track;
}

/** The arguments of `play_next`. */
export interface PlayNextArgs {
  t: Track;
}

/** The arguments of `hide_rec`. */
export interface HideRecArgs {
  t: Track;
}

/** The arguments of `restore_rec`. */
export interface RestoreRecArgs {
  t: Track;
}

/** The arguments of `note_failure`. */
export interface NoteFailureArgs {
  reason: string;
}

/** The arguments of `new_mixtape`. */
export interface NewMixtapeArgs {
  name: string;
}

/** The arguments of `update_taste`. */
export interface UpdateTasteArgs {
  which: string;
  on: boolean;
}

/** The arguments of `dismiss_artist`. */
export interface DismissArtistArgs {
  a: Artist;
}

/** The arguments of `prefer_artist`. */
export interface PreferArtistArgs {
  a: Artist;
}

/** The arguments of `queue_moved`. */
export interface QueueMovedArgs {
  p: unknown;
  ids: string[];
}

/** The arguments of `like_playing`. */
export interface LikePlayingArgs {
  p: unknown;
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

/** Track — `tl.so.tilo.music.Track`. */
export interface Track extends tilo.Resource {
  wapyyydf: string;  // key
  zpnajesf: string;  // title
  pzaz5i7w: string;  // artist
  kyegi6xm: string;  // artist_key
  j67d2q4o: string;  // album
  ojqrhkks: string;  // album_key
  "2sm6xpgo": number;  // seconds
  wy2gg3hw: number;  // plays
  yaujsp7u: string;  // cover
  hlfttyoj: string;  // found_for
  "47b7sivc": string;  // stream
  fdkr4wxf: string;  // offline
  "2hvaqxk2": string;  // lyrics
  "4hjvzjcl": string;  // lyrics_state
  wt3jlodv: boolean;  // liked
  /** Plays one song, alone: what a row's press does. The place is the shared session's own record, so the song continues on this device or another. */
  play_song(): Promise<void>;
  /** Plays a suggested song. */
  play_rec(): Promise<void>;
  /** Likes a song, or takes the like back. */
  like(args?: {
    liked?: boolean;
  }): Promise<void>;
  /** Runs `like` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  likeMany(items: LikeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps a song on this device, for playing with no network. */
  keep_offline(): Promise<void>;
  /** Takes the kept copy away; the song still plays from the catalogue. */
  remove_offline(): Promise<void>;
  /** Runs `remove_offline` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_offlineMany(items: RemoveOfflineArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Track` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TRACK_ACTIONS = ["play_song", "play_rec", "like", "likeMany", "keep_offline", "remove_offline", "remove_offlineMany"] as const;

/** The `tl.so.tilo.music.Track` kind at `path`. */
export function track(ws: tilo.Workspace, path: string): Track {
  return resourceProxy(ws, path, SURFACE, TRACK_ACTIONS) as unknown as Track;
}

/** Album — `tl.so.tilo.music.Album`. */
export interface Album extends tilo.Resource {
  xlmick3d: string;  // key
  ui7q6nc2: string;  // title
  c36cztfw: string;  // artist
  "2px6jkdl": string;  // artist_key
  aecqneo6: number;  // year
  aajvirg5: string;  // release_kind
  lzsbpa3s: string;  // cover
  hsjkx4hy: string;  // found_for
  "6ygyv2n2": boolean;  // in_library
  /** Plays an album's songs from the top. */
  play_album(): Promise<void>;
  /** Runs `play_album` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  play_albumMany(items: PlayAlbumArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps an album in the library, or takes it out. */
  save_album(args?: {
    keep?: boolean;
  }): Promise<void>;
  /** Runs `save_album` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  save_albumMany(items: SaveAlbumArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Album` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ALBUM_ACTIONS = ["play_album", "play_albumMany", "save_album", "save_albumMany"] as const;

/** The `tl.so.tilo.music.Album` kind at `path`. */
export function album(ws: tilo.Workspace, path: string): Album {
  return resourceProxy(ws, path, SURFACE, ALBUM_ACTIONS) as unknown as Album;
}

/** Artist — `tl.so.tilo.music.Artist`. */
export interface Artist extends tilo.Resource {
  a3jwtsne: string;  // key
  aht5vji7: string;  // name
  p7suhaoo: number;  // subscribers
  xmevyzrv: string;  // avatar
  aazaiqyu: string;  // found_for
  wkrm32am: boolean;  // followed
  /** Follows an artist. */
  follow_artist(args?: {
    followed?: boolean;
  }): Promise<void>;
  /** Runs `follow_artist` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  follow_artistMany(items: FollowArtistArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks the catalogue for an artist's page. */
  refresh_artist(): Promise<void>;
}

/** The actions `Artist` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ARTIST_ACTIONS = ["follow_artist", "follow_artistMany", "refresh_artist"] as const;

/** The `tl.so.tilo.music.Artist` kind at `path`. */
export function artist(ws: tilo.Workspace, path: string): Artist {
  return resourceProxy(ws, path, SURFACE, ARTIST_ACTIONS) as unknown as Artist;
}

/** Playlist — `tl.so.tilo.music.Playlist`. */
export interface Playlist extends tilo.Resource {
  zliedtfz: string;  // key
  rtkxqj2p: string;  // title
  htibtwee: string;  // subtitle
  "4rfi4yqu": number;  // track_count
  qdbwyy7o: string;  // cover
  ac7ep6rb: string;  // found_for
  /** Asks the catalogue for a playlist's songs. */
  open_playlist(): Promise<void>;
}

/** The actions `Playlist` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PLAYLIST_ACTIONS = ["open_playlist"] as const;

/** The `tl.so.tilo.music.Playlist` kind at `path`. */
export function playlist(ws: tilo.Workspace, path: string): Playlist {
  return resourceProxy(ws, path, SURFACE, PLAYLIST_ACTIONS) as unknown as Playlist;
}

/** SpotifyDevice — `tl.so.tilo.music.SpotifyDevice`. */
export interface SpotifyDevice extends tilo.Resource {
  "4ruem7bk": string;  // key
  fysa7usi: string;  // name
  ajgkhxtc: string;  // kind
  njju5rcv: boolean;  // active
  daqeango: number;  // volume
}

/** The actions `SpotifyDevice` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SPOTIFYDEVICE_ACTIONS = [] as const;

/** The `tl.so.tilo.music.SpotifyDevice` kind at `path`. */
export function spotifyDevice(ws: tilo.Workspace, path: string): SpotifyDevice {
  return resourceProxy(ws, path, SURFACE, SPOTIFYDEVICE_ACTIONS) as unknown as SpotifyDevice;
}

/** Section — `tl.so.tilo.music.Section`. */
export interface Section extends tilo.Resource {
  jsrmddga: string;  // key
  "2kqqdved": string;  // title
  lp6qsyhj: number;  // order
}

/** The actions `Section` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SECTION_ACTIONS = [] as const;

/** The `tl.so.tilo.music.Section` kind at `path`. */
export function section(ws: tilo.Workspace, path: string): Section {
  return resourceProxy(ws, path, SURFACE, SECTION_ACTIONS) as unknown as Section;
}

/** Suggestion — `tl.so.tilo.music.Suggestion`. */
export interface Suggestion extends tilo.Resource {
  qh34h2ch: string;  // key
  xh6birha: string;  // section
  a5bkjkyq: string;  // kind
  gqxd4uhg: string;  // target
  "5a3o3zds": string;  // title
  oeqw4xpb: string;  // subtitle
  "5gpg6lqq": string;  // cover
}

/** The actions `Suggestion` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SUGGESTION_ACTIONS = [] as const;

/** The `tl.so.tilo.music.Suggestion` kind at `path`. */
export function suggestion(ws: tilo.Workspace, path: string): Suggestion {
  return resourceProxy(ws, path, SURFACE, SUGGESTION_ACTIONS) as unknown as Suggestion;
}

/** Mixtape — `tl.so.tilo.music.Mixtape`. */
export interface Mixtape extends tilo.Resource {
  "5d2oxk4b": string;  // name
  dombkrij: Array<tilo.Entry<unknown>>;  // tracks
  jonjr2zv: string;  // cover
  /** Plays a mixtape's songs in the person's order. */
  play_mixtape(): Promise<void>;
  /** Runs `play_mixtape` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  play_mixtapeMany(items: PlayMixtapeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds a song to a mixtape. */
  mixtape_add(args: {
    t: Track;
  }): Promise<void>;
  /** Runs `mixtape_add` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  mixtape_addMany(items: MixtapeAddArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes a song out of a mixtape. */
  mixtape_remove(args: {
    t: Track;
  }): Promise<void>;
  /** Runs `mixtape_remove` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  mixtape_removeMany(items: MixtapeRemoveArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Gives a mixtape the cover the person picks. */
  pick_cover(args: {
    file: unknown;
  }): Promise<void>;
  /** Runs `pick_cover` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  pick_coverMany(items: PickCoverArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets a mixtape's cover to the picture the person chose. */
  set_cover(args: {
    file: unknown;
  }): Promise<void>;
  /** Runs `set_cover` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_coverMany(items: SetCoverArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Mixtape` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MIXTAPE_ACTIONS = ["play_mixtape", "play_mixtapeMany", "mixtape_add", "mixtape_addMany", "mixtape_remove", "mixtape_removeMany", "pick_cover", "pick_coverMany", "set_cover", "set_coverMany"] as const;

/** The `tl.so.tilo.music.Mixtape` kind at `path`. */
export function mixtape(ws: tilo.Workspace, path: string): Mixtape {
  return resourceProxy(ws, path, SURFACE, MIXTAPE_ACTIONS) as unknown as Mixtape;
}

/** Taste — `tl.so.tilo.music.Taste`. */
export interface Taste extends tilo.Resource {
  nhxvxvrt: boolean;  // followed_artists
  fygjzxca: boolean;  // liked_songs
  splmmoir: boolean;  // saved_albums
  gfhj3dh4: string[];  // dismissed
  oa6ryv4n: string[];  // preferred
  nsvv7dnn: string[];  // hidden
}

/** The actions `Taste` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TASTE_ACTIONS = [] as const;

/** The `tl.so.tilo.music.Taste` kind at `path`. */
export function taste(ws: tilo.Workspace, path: string): Taste {
  return resourceProxy(ws, path, SURFACE, TASTE_ACTIONS) as unknown as Taste;
}

/** Library — `tl.so.tilo.music.Library`. */
export interface Library extends tilo.Resource {
  hkr3ia6d: string;  // name
  cp5uymkx: Array<tilo.Entry<Track>>;  // tracks
  phkx3ocv: Array<tilo.Entry<Album>>;  // albums
  oarbmgph: Array<tilo.Entry<Artist>>;  // artists
  bfjw6uca: Array<tilo.Entry<Playlist>>;  // playlists
  k6usb4n2: Array<tilo.Entry<Suggestion>>;  // suggestions
  qu6nxpq3: Array<tilo.Entry<Section>>;  // sections
  bfayjji6: Array<tilo.Entry<Mixtape>>;  // mixtapes
  u4kuzao5: Taste;  // taste
  scejauac: unknown | null;  // playing
  tnkopk2m: Array<tilo.Entry<unknown>>;  // sessions
  cjoykprs: Array<tilo.Entry<SpotifyDevice>>;  // devices
  d6agrwhu: string;  // spotify_state
  wzbxwvai: string;  // spotify_account
  q6fvzfwi: string;  // spotify_product
  bnnshdna: string;  // spotify_trouble
  rwf7lfs6: string;  // connect_url
  gghnswba: string;  // spotify_active
  sdkihe6k: string;  // spotify_now
  "22kwzmah": boolean;  // spotify_playing
  wout6rgv: string;  // catalogue
  olobgipa: string;  // search_state
  foqrtqa3: string;  // search_trouble
  w3c7fplo: string;  // home_state
  gmizjqwj: string;  // home_trouble
  vnufl2py: string;  // trouble
  fkvdcomi: boolean;  // online
  /** The session this app plays through: the one it has, or one made now. It is kept both in `playing`, which a view reads, and in `sessions`, so the record is a resource of its own. */
  session(): Promise<unknown>;
  /** Opens an audio file: the file a person picked, or one Files handed over, and plays it. The song it already made opens instead of a second copy, so its place in history stays one. */
  open_audio_file(args: {
    file: unknown;
  }): Promise<Track>;
  /** Adds a song to what follows. */
  add_to_queue(args: {
    t: Track;
  }): Promise<void>;
  /** Runs `add_to_queue` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  add_to_queueMany(items: AddToQueueArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Queues a song right after what plays. */
  play_next(args: {
    t: Track;
  }): Promise<void>;
  /** Runs `play_next` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  play_nextMany(items: PlayNextArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks for the words of what plays, keeping the session it asks for. */
  lyrics_now(): Promise<void>;
  /** Hides a song from the suggestions. History can undo it, and the taste page brings it back. */
  hide_rec(args: {
    t: Track;
  }): Promise<void>;
  /** Runs `hide_rec` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  hide_recMany(items: HideRecArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Brings a hidden song back into the suggestions. */
  restore_rec(args: {
    t: Track;
  }): Promise<void>;
  /** Runs `restore_rec` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  restore_recMany(items: RestoreRecArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks the catalogue for the songs, albums, artists and playlists a query finds. */
  search(args: {
    query: string;
  }): Promise<void>;
  // the app's `refresh` action is the SDK's own: run it with `action("refresh")`.
  /** Does what a suggestion on the home page names. */
  open_suggestion(args: {
    row: Suggestion;
  }): Promise<void>;
  /** Asks for what plays to be sung, into the song's own record. */
  lyrics_of(args: {
    p: unknown;
  }): Promise<void>;
  /** Says why the player stopped. */
  note_failure(args: {
    reason: string;
  }): Promise<void>;
  /** Runs `note_failure` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  note_failureMany(items: NoteFailureArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Makes a playlist of the person's own. */
  new_mixtape(args: {
    name: string;
  }): Promise<void>;
  /** Runs `new_mixtape` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  new_mixtapeMany(items: NewMixtapeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Chooses which catalogue the app asks. */
  set_catalogue(args: {
    which: string;
  }): Promise<void>;
  /** Connects the person's Spotify: the page they sign in on is shown in the Spotify section. */
  connect_spotify(): Promise<void>;
  /** Takes the sign-in the person gave on Spotify's page. */
  finish_spotify(): Promise<void>;
  /** Forgets this device's Spotify sign-in. */
  disconnect_spotify(): Promise<void>;
  /** Reads the places Spotify plays on. */
  spotify_devices(): Promise<void>;
  /** Moves what plays to `d`, from the beginning. */
  transfer_playback(args: {
    d: SpotifyDevice;
  }): Promise<void>;
  /** Plays `uri` on Spotify, where it plays: empty plays what was playing. */
  remote_play(args?: {
    uri?: string;
  }): Promise<void>;
  /** Pauses what plays on Spotify. */
  remote_pause(): Promise<void>;
  /** Plays what follows on Spotify. */
  remote_next(): Promise<void>;
  /** Plays what played before on Spotify. */
  remote_previous(): Promise<void>;
  /** Reads what plays on Spotify now. */
  spotify_now(): Promise<void>;
  /** Turns one of the recommendations' switches. */
  update_taste(args: {
    which: string;
    on: boolean;
  }): Promise<void>;
  /** Runs `update_taste` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  update_tasteMany(items: UpdateTasteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Leaves an artist out of the recommendations, or brings them back. */
  dismiss_artist(args: {
    a: Artist;
  }): Promise<void>;
  /** Runs `dismiss_artist` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  dismiss_artistMany(items: DismissArtistArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks for more of an artist. */
  prefer_artist(args: {
    a: Artist;
  }): Promise<void>;
  /** Runs `prefer_artist` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  prefer_artistMany(items: PreferArtistArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Library` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LIBRARY_ACTIONS = ["session", "open_audio_file", "add_to_queue", "add_to_queueMany", "play_next", "play_nextMany", "lyrics_now", "hide_rec", "hide_recMany", "restore_rec", "restore_recMany", "search", "open_suggestion", "lyrics_of", "note_failure", "note_failureMany", "new_mixtape", "new_mixtapeMany", "set_catalogue", "connect_spotify", "finish_spotify", "disconnect_spotify", "spotify_devices", "transfer_playback", "remote_play", "remote_pause", "remote_next", "remote_previous", "spotify_now", "update_taste", "update_tasteMany", "dismiss_artist", "dismiss_artistMany", "prefer_artist", "prefer_artistMany"] as const;

/** The `tl.so.tilo.music.Library` kind at `path`. */
export function library(ws: tilo.Workspace, path: string): Library {
  return resourceProxy(ws, path, SURFACE, LIBRARY_ACTIONS) as unknown as Library;
}

/** Music — `tl.so.tilo.music`. */
export interface Music extends tilo.Resource {
  /** Moves the dragged row where it was dropped: the two ids its reorder carries. */
  queue_moved(args: {
    p: unknown;
    ids: string[];
  }): Promise<void>;
  /** Runs `queue_moved` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  queue_movedMany(items: QueueMovedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Likes what plays. */
  like_playing(args: {
    p: unknown;
    liked?: boolean;
  }): Promise<void>;
  /** Runs `like_playing` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  like_playingMany(items: LikePlayingArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Music` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MUSIC_ACTIONS = ["queue_moved", "queue_movedMany", "like_playing", "like_playingMany"] as const;

/** The `tl.so.tilo.music` kind at `path`. */
export function music(ws: tilo.Workspace, path: string): Music {
  return resourceProxy(ws, path, SURFACE, MUSIC_ACTIONS) as unknown as Music;
}

