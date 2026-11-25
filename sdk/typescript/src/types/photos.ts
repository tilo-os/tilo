/**
 * Types for Photos (`so.tilo.photos`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`photos/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.photos@0d971e43f3ec05f8";

/** What an edit does to one field of every selected photo: keeps what each photo has, takes it off all of them, gives them all one value, or adds or takes off names (tags, people). One of "keep", "clear", "set", "add", "remove". */
export type Change = "keep" | "clear" | "set" | "add" | "remove";

/** The arguments of `check_picture`. */
export interface CheckPictureArgs {
}

/** The arguments of `toggle_favorite`. */
export interface ToggleFavoriteArgs {
  favorite?: boolean;
}

/** The arguments of `rotate_right`. */
export interface RotateRightArgs {
}

/** The arguments of `rotate_left`. */
export interface RotateLeftArgs {
}

/** The arguments of `reset_edits`. */
export interface ResetEditsArgs {
}

/** The arguments of `develop`. */
export interface DevelopArgs {
  rotation: number;
  crop: number[];
}

/** The arguments of `recognize_text`. */
export interface RecognizeTextArgs {
}

/** The arguments of `crop_photo`. */
export interface CropPhotoArgs {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** The arguments of `tag`. */
export interface TagArgs {
  t: string;
}

/** The arguments of `untag`. */
export interface UntagArgs {
  t: string;
}

/** The arguments of `set_place`. */
export interface SetPlaceArgs {
  at: number[];
}

/** The arguments of `clear_place`. */
export interface ClearPlaceArgs {
}

/** The arguments of `add_selected_to_album`. */
export interface AddSelectedToAlbumArgs {
  ids: string[];
}

/** The arguments of `rename_album`. */
export interface RenameAlbumArgs {
  to: string;
}

/** The arguments of `album_contains_toggle`. */
export interface AlbumContainsToggleArgs {
  p: Photo;
}

/** The arguments of `import_folder`. */
export interface ImportFolderArgs {
  files: unknown[];
}

/** The arguments of `start_import`. */
export interface StartImportArgs {
  ask: string;
  found: number;
}

/** The arguments of `imported`. */
export interface ImportedArgs {
  j: unknown;
}

/** The arguments of `retry_import`. */
export interface RetryImportArgs {
}

/** The arguments of `reindex`. */
export interface ReindexArgs {
  folder?: string;
}

/** The arguments of `dismiss_import`. */
export interface DismissImportArgs {
}

/** The arguments of `favorite_selected`. */
export interface FavoriteSelectedArgs {
  ids: string[];
}

/** The arguments of `remove_selected`. */
export interface RemoveSelectedArgs {
  ids: string[];
}

/** The arguments of `edit_selected`. */
export interface EditSelectedArgs {
  ids: string[];
  tags?: Change;
  tag_names?: string[];
  tags_off?: string[];
  people?: Change;
  person_names?: string[];
  people_off?: string[];
  folder?: Change;
  folder_text?: string;
  captured_at?: Change;
  captured_text?: string;
  description?: Change;
  description_text?: string;
  place?: Change;
  place_name?: string;
  latitude?: number;
  longitude?: number;
}

/** The arguments of `remove_album`. */
export interface RemoveAlbumArgs {
  a: Album;
}

/** The arguments of `start_album_draft`. */
export interface StartAlbumDraftArgs {
  id: string;
}

/** The arguments of `album_draft_toggle`. */
export interface AlbumDraftToggleArgs {
  p: Photo;
}

/** The arguments of `cancel_album_draft`. */
export interface CancelAlbumDraftArgs {
}

/** The arguments of `save_album`. */
export interface SaveAlbumArgs {
}

/** The arguments of `delete_album_draft`. */
export interface DeleteAlbumDraftArgs {
}

/** The arguments of `dismiss_notice`. */
export interface DismissNoticeArgs {
}

/** The arguments of `add_person`. */
export interface AddPersonArgs {
  p: Photo;
  person: string;
}

/** The arguments of `remove_person`. */
export interface RemovePersonArgs {
  p: Photo;
  person: string;
}

/** The arguments of `rename_person`. */
export interface RenamePersonArgs {
  person: Person;
  to: string;
}

/** The arguments of `set_recognition`. */
export interface SetRecognitionArgs {
  on: boolean;
}

/** The arguments of `pause_scan`. */
export interface PauseScanArgs {
}

/** The arguments of `delete_face_data`. */
export interface DeleteFaceDataArgs {
}

/** The arguments of `approve_suggestion`. */
export interface ApproveSuggestionArgs {
  s: Suggestion;
  name?: string;
}

/** The arguments of `merge_suggestion`. */
export interface MergeSuggestionArgs {
  s: Suggestion;
  person: Person;
}

/** The arguments of `dismiss_suggestion`. */
export interface DismissSuggestionArgs {
  s: Suggestion;
}

/** The arguments of `merge_people`. */
export interface MergePeopleArgs {
  from: Person;
  into: Person;
}

/** The arguments of `split_photo`. */
export interface SplitPhotoArgs {
  person: Person;
  photo: string;
}

/** The arguments of `remove_person_resource`. */
export interface RemovePersonResourceArgs {
  person: Person;
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

/** Photo — `tl.so.tilo.photos.Photo`. */
export interface Photo extends tilo.Resource {
  rv4vs7su: string;  // name
  r55t3rg3: string;  // picture
  vwrtdas3: string;  // thumb
  "2i7i3as5": string;  // edited
  ktv6ci4q: number;  // rotation
  dpfjny5e: number[];  // crop
  nyatlpd5: string;  // picture_state
  yyfm6s7t: boolean;  // favorite
  wm3tr75p: string;  // captured_at
  zz3fpxbt: string;  // folder
  "5bhxlhcf": string[];  // tags
  vlem53gy: string[];  // people
  bgo4zgpq: string;  // location
  kko7mkgg: number;  // latitude
  "4xox6lum": number;  // longitude
  brtcqh34: string;  // description
  p4pebna2: string;  // recognized
  "6esqywn4": string[];  // objects
  "7rzyj24a": string[];  // pets
  /** Looks at where this device stands on a photo's picture: whether it holds the bytes yet, and whether what it holds is a picture it opens. A record reaches this device from another one before its picture's bytes do, so a photo says the picture is still arriving instead of drawing nothing; the window looks again while it waits. */
  check_picture(): Promise<void>;
  /** Runs `check_picture` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  check_pictureMany(items: CheckPictureArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Marks a photo as a favorite, or takes that back. */
  toggle_favorite(args?: {
    favorite?: boolean;
  }): Promise<void>;
  /** Runs `toggle_favorite` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  toggle_favoriteMany(items: ToggleFavoriteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Turns the photograph a quarter clockwise, as its person sees it. */
  rotate_right(): Promise<void>;
  /** Runs `rotate_right` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rotate_rightMany(items: RotateRightArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Turns the photograph a quarter anticlockwise. */
  rotate_left(): Promise<void>;
  /** Runs `rotate_left` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rotate_leftMany(items: RotateLeftArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts the photograph back as it came, taking every edit off it. */
  reset_edits(): Promise<void>;
  /** Runs `reset_edits` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  reset_editsMany(items: ResetEditsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Records an edit on the photograph and draws it: the device keeps the part the person chose, then turns it, and keeps the result in the content store. Nothing is written over the photograph, so every edit can be taken back and the person can see the picture as it came beside the one they made. */
  develop(args: {
    rotation: number;
    crop: number[];
  }): Promise<void>;
  /** Runs `develop` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  developMany(items: DevelopArgs[]): Promise<tilo.ActionAnswer[]>;
  /** What a picture shows, in a sentence, as the device's image model reads it: the sentence the library's details show and search finds. The work is the device's own, asked of this session on the app's behalf, so the app never runs a model itself. */
  describe_photo(): Promise<void>;
  /** The words in the photograph, read on this device and kept with the photo, so the library finds it by what it says. Needs no model: a device without one still reads its text. */
  recognize_text(): Promise<void>;
  /** Runs `recognize_text` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  recognize_textMany(items: RecognizeTextArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Copies the photograph to the person's clipboard as a picture, so it pastes into another app. */
  copy_photo(): Promise<void>;
  /** What the device's image model finds in a picture, added to what the photo already lists: the objects, and the pets apart from them. */
  tag_photo(): Promise<void>;
  /** What the photograph shows, as the device's model reads it in one go: the sentence search finds it by, the words in it, and the objects and pets it holds. It replaces what the photo said before, so analyzing again after the picture changed says what it shows now. */
  analyze_photo(): Promise<void>;
  /** Keeps the part of the photograph its person chose: `x`, `y`, `width` and `height` are fractions of the picture as the window shows it, so every member and device reads the crop. */
  crop_photo(args: {
    x: number;
    y: number;
    width: number;
    height: number;
  }): Promise<void>;
  /** Runs `crop_photo` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  crop_photoMany(items: CropPhotoArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Gives a photo a tag. */
  tag(args: {
    t: string;
  }): Promise<void>;
  /** Runs `tag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  tagMany(items: TagArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes a tag off a photo. */
  untag(args: {
    t: string;
  }): Promise<void>;
  /** Runs `untag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  untagMany(items: UntagArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The link that reaches a photo's picture: the address of its bytes, which every device of the workspace reads from the content store. */
  share(): Promise<string>;
  /** Sets the place a photo was taken from where the person points on the map: the picker reports the point as its latitude then its longitude. */
  set_place(args: {
    at: number[];
  }): Promise<void>;
  /** Runs `set_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_placeMany(items: SetPlaceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes the place off a photo. */
  clear_place(): Promise<void>;
  /** Runs `clear_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_placeMany(items: ClearPlaceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The original picture as a file, to save where the person says. */
  export_photo(): Promise<unknown>;
}

/** The actions `Photo` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PHOTO_ACTIONS = ["check_picture", "check_pictureMany", "toggle_favorite", "toggle_favoriteMany", "rotate_right", "rotate_rightMany", "rotate_left", "rotate_leftMany", "reset_edits", "reset_editsMany", "develop", "developMany", "describe_photo", "recognize_text", "recognize_textMany", "copy_photo", "tag_photo", "analyze_photo", "crop_photo", "crop_photoMany", "tag", "tagMany", "untag", "untagMany", "share", "set_place", "set_placeMany", "clear_place", "clear_placeMany", "export_photo"] as const;

/** The `tl.so.tilo.photos.Photo` kind at `path`. */
export function photo(ws: tilo.Workspace, path: string): Photo {
  return resourceProxy(ws, path, SURFACE, PHOTO_ACTIONS) as unknown as Photo;
}

/** Album — `tl.so.tilo.photos.Album`. */
export interface Album extends tilo.Resource {
  unujaudz: string;  // name
  k7fl2y27: string[];  // photos
  /** Puts each named photo in an album. */
  add_selected_to_album(args: {
    ids: string[];
  }): Promise<void>;
  /** Runs `add_selected_to_album` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  add_selected_to_albumMany(items: AddSelectedToAlbumArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Renames an album. */
  rename_album(args: {
    to: string;
  }): Promise<void>;
  /** Runs `rename_album` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rename_albumMany(items: RenameAlbumArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts a photo in an album, or takes it out of one when it is already in it. */
  album_contains_toggle(args: {
    p: Photo;
  }): Promise<void>;
  /** Runs `album_contains_toggle` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  album_contains_toggleMany(items: AlbumContainsToggleArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Album` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ALBUM_ACTIONS = ["add_selected_to_album", "add_selected_to_albumMany", "rename_album", "rename_albumMany", "album_contains_toggle", "album_contains_toggleMany"] as const;

/** The `tl.so.tilo.photos.Album` kind at `path`. */
export function album(ws: tilo.Workspace, path: string): Album {
  return resourceProxy(ws, path, SURFACE, ALBUM_ACTIONS) as unknown as Album;
}

/** Person — `tl.so.tilo.photos.Person`. */
export interface Person extends tilo.Resource {
  segahwtp: string;  // name
  vbzsnqg5: string[];  // photos
}

/** The actions `Person` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PERSON_ACTIONS = [] as const;

/** The `tl.so.tilo.photos.Person` kind at `path`. */
export function person(ws: tilo.Workspace, path: string): Person {
  return resourceProxy(ws, path, SURFACE, PERSON_ACTIONS) as unknown as Person;
}

/** Suggestion — `tl.so.tilo.photos.Suggestion`. */
export interface Suggestion extends tilo.Resource {
  qracadw6: string;  // name
  ehc6rrhm: string[];  // photos
  "2adnxzro": boolean;  // sent
}

/** The actions `Suggestion` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SUGGESTION_ACTIONS = [] as const;

/** The `tl.so.tilo.photos.Suggestion` kind at `path`. */
export function suggestion(ws: tilo.Workspace, path: string): Suggestion {
  return resourceProxy(ws, path, SURFACE, SUGGESTION_ACTIONS) as unknown as Suggestion;
}

/** Library — `tl.so.tilo.photos.Library`. */
export interface Library extends tilo.Resource {
  moeuycny: Array<tilo.Entry<Photo>>;  // photos
  tqa4lyow: Array<tilo.Entry<Album>>;  // albums
  xr5mtp2m: Array<tilo.Entry<Person>>;  // people
  r7k2giak: Array<tilo.Entry<Suggestion>>;  // suggestions
  tp7e7pzd: number;  // import_found
  "6jdgyzo3": number;  // import_done
  ybraeglq: string[];  // import_issues
  "4gwd7453": string;  // import_job
  ii2qmjt5: boolean;  // import_stopped
  xkkw7d65: Array<tilo.Entry<Unread>>;  // unread
  fypuuz3p: Array<tilo.Entry<FaceScan>>;  // face_scans
  jcilt5zf: string;  // notice
  nyozkafm: Array<tilo.Entry<AlbumDraft>>;  // album_drafts
  /** Imports a photograph the person picks: the library keeps the address of its bytes, the name it came in with, what its own container says of it (when and where it was taken) and a small preview the grid draws. A picture whose bytes the window already holds reads at once. */
  import_photo(args: {
    file: unknown;
  }): Promise<Photo>;
  /** Adds the picture a file holds to the library: the photo the same picture already made, brought up to what the file says now, or a new one. What the person set on a photo — its place, its tags — is theirs, so reading the file again never overwrites it. */
  absorb_picture(args: {
    file: unknown;
  }): Promise<Photo>;
  /** Imports a photograph a caller already knows the capture time and place of (the camera's handoff). It opens no file kind of its own: the `opens` action above is what Files and the person's own file system hand a picture to. */
  import_photo_meta(args: {
    file: unknown;
    captured_at?: string;
    location?: string;
  }): Promise<Photo>;
  /** Imports the pictures a person drops on the window or picks a folder of, as a job on this device: it reads each picture, when and where it was taken and its preview, saying how far it has got as it goes, and can be stopped. A file that is no picture is left alone and said so. Picking the same folder again brings what the files say to the photos already there, rather than making a second copy of each. */
  import_folder(args: {
    files: unknown[];
  }): Promise<void>;
  /** Runs `import_folder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  import_folderMany(items: ImportFolderArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Starts the device reading the pictures `ask` names, stopping an import still under way first. When the job ends, `imported` brings what it read into the library, as the one who started it. */
  start_import(args: {
    ask: string;
    found: number;
  }): Promise<void>;
  /** Runs `start_import` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  start_importMany(items: StartImportArgs[]): Promise<tilo.ActionAnswer[]>;
  /** An import's end: each picture it read becomes a photo, or brings what it says to the photo the same picture already made, and what it could not read is kept to try again. What the person set on a photo (its time, its place) is theirs, so reading it again never overwrites it. */
  imported(args: {
    j: unknown;
  }): Promise<void>;
  /** Runs `imported` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  importedMany(items: ImportedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Stops the import under way: it ends after the picture it is reading, and what it read so far joins the library. */
  stop_import(): Promise<void>;
  /** Tries again the pictures the last import could not read where trying again could help. */
  retry_import(): Promise<void>;
  /** Runs `retry_import` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  retry_importMany(items: RetryImportArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reads the photos of a folder again, or of the whole library when no folder is named: their previews, and when and where they were taken where nobody set it. */
  reindex(args?: {
    folder?: string;
  }): Promise<void>;
  /** Runs `reindex` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  reindexMany(items: ReindexArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes the last import's report off the window. */
  dismiss_import(): Promise<void>;
  /** Runs `dismiss_import` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  dismiss_importMany(items: DismissImportArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Marks each named photo a favorite. */
  favorite_selected(args: {
    ids: string[];
  }): Promise<void>;
  /** Runs `favorite_selected` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  favorite_selectedMany(items: FavoriteSelectedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes each named photo out of the library. */
  remove_selected(args: {
    ids: string[];
  }): Promise<void>;
  /** Runs `remove_selected` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_selectedMany(items: RemoveSelectedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Changes the details of every named photo as one change: a field left `keep` keeps what each photo has, `clear` takes it off all of them, `set` gives them all one value, and tags and people are added or taken off across the selection, so every photo keeps the rest of its own. The selection is names in a list, so a person's selection, an agent's and a program's are the same change, undone at once. */
  edit_selected(args: {
    ids: string[];
    tags?: Change;
    tag_names?: string[];
    tags_off?: string[];
    people?: Change;
    person_names?: string[];
    people_off?: string[];
    folder?: Change;
    folder_text?: string;
    captured_at?: Change;
    captured_text?: string;
    description?: Change;
    description_text?: string;
    place?: Change;
    place_name?: string;
    latitude?: number;
    longitude?: number;
  }): Promise<void>;
  /** Runs `edit_selected` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  edit_selectedMany(items: EditSelectedArgs[]): Promise<tilo.ActionAnswer[]>;
  // the app's `remove` action is the SDK's own: run it with `action("remove")`.
  /** Starts an album, and puts the photo it is started from in it. */
  new_album(args?: {
    p?: Photo;
  }): Promise<Album>;
  /** Deletes an album. Its photos stay in the library. */
  remove_album(args: {
    a: Album;
  }): Promise<void>;
  /** Runs `remove_album` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_albumMany(items: RemoveAlbumArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens an album draft: the album `id` names as it stands now, or an empty one to start. The person chooses its photos in the library and nothing about the album changes until they save the draft, so a change another member makes meanwhile is reviewed, not overwritten unseen. */
  start_album_draft(args: {
    id: string;
  }): Promise<void>;
  /** Runs `start_album_draft` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  start_album_draftMany(items: StartAlbumDraftArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Chooses a photo for the album draft, or takes it out of the draft when it is already in it. */
  album_draft_toggle(args: {
    p: Photo;
  }): Promise<void>;
  /** Runs `album_draft_toggle` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  album_draft_toggleMany(items: AlbumDraftToggleArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Closes the album draft, saving nothing. */
  cancel_album_draft(): Promise<void>;
  /** Runs `cancel_album_draft` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  cancel_album_draftMany(items: CancelAlbumDraftArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Saves the album draft. A photo the draft names that the library no longer holds, or an album another member deleted while it was open, is a change made first by someone else: the draft stays as it is and says so, so they review it and save again. What the save does add and take away merges with a change another member made meanwhile. */
  save_album(): Promise<void>;
  /** Runs `save_album` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  save_albumMany(items: SaveAlbumArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes the album the draft is of. Its photos stay in the library. */
  delete_album_draft(): Promise<void>;
  /** Runs `delete_album_draft` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_album_draftMany(items: DeleteAlbumDraftArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes down what the library last had to say. */
  dismiss_notice(): Promise<void>;
  /** Runs `dismiss_notice` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  dismiss_noticeMany(items: DismissNoticeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Names a person on a photo, and puts the photo on that person when the library knows them. */
  add_person(args: {
    p: Photo;
    person: string;
  }): Promise<void>;
  /** Runs `add_person` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  add_personMany(items: AddPersonArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes a person off a photo, and the photo off that person. */
  remove_person(args: {
    p: Photo;
    person: string;
  }): Promise<void>;
  /** Runs `remove_person` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_personMany(items: RemovePersonArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Names a person: the photos they are on say the new name too. */
  rename_person(args: {
    person: Person;
    to: string;
  }): Promise<void>;
  /** Runs `rename_person` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rename_personMany(items: RenamePersonArgs[]): Promise<tilo.ActionAnswer[]>;
  /** This person's face scan, started where they have none. */
  face_scan(): Promise<FaceScan>;
  /** Lets this device look for faces in the photos, or stops it: turning it off ends a scan under way, and keeps every name already given. */
  set_recognition(args: {
    on: boolean;
  }): Promise<void>;
  /** Runs `set_recognition` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_recognitionMany(items: SetRecognitionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Starts scanning the photos on this device for faces, or goes on with a paused scan: the model reads twelve at a time, and the window takes the next twelve as each batch ends, until every photo nobody is named on was read. Each face it finds is a suggestion of the scanner's own. */
  scan_faces(): Promise<void>;
  /** One batch of a running scan: the next twelve photos it has not read. */
  scan_step(): Promise<void>;
  /** Reads the next twelve photos `scan` has not read and nobody is named on yet. Answers whether there were any to read. */
  read_batch(args: {
    scan: FaceScan;
  }): Promise<boolean>;
  /** Pauses the scan under way: scanning again goes on with the next photos. */
  pause_scan(): Promise<void>;
  /** Runs `pause_scan` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  pause_scanMany(items: PauseScanArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes the face data this person's scans made on this device: the faces nobody named yet and what the scan had read. The people already named are kept. */
  delete_face_data(): Promise<void>;
  /** Runs `delete_face_data` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_face_dataMany(items: DeleteFaceDataArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Names a face: its photos join the person with that name, and the suggestion is sent, so the workspace keeps it. */
  approve_suggestion(args: {
    s: Suggestion;
    name?: string;
  }): Promise<void>;
  /** Runs `approve_suggestion` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  approve_suggestionMany(items: ApproveSuggestionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts a face's photos on a person the library already knows, and sends the suggestion. */
  merge_suggestion(args: {
    s: Suggestion;
    person: Person;
  }): Promise<void>;
  /** Runs `merge_suggestion` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  merge_suggestionMany(items: MergeSuggestionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes a suggestion away: the face is not offered again, since the photos are not rescanned into one while they are unnamed only by choice. */
  dismiss_suggestion(args: {
    s: Suggestion;
  }): Promise<void>;
  /** Runs `dismiss_suggestion` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  dismiss_suggestionMany(items: DismissSuggestionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The person the library knows by `name`, started where it knows none. */
  person_named(args: {
    name: string;
  }): Promise<Person>;
  /** Starts a person the library knows, with the photos it already has of them. */
  new_person(args: {
    name: string;
    photos: string[];
  }): Promise<Person>;
  /** Merges one person into another: their photos come together under the name kept, and the name merged away leaves every photo it was on. */
  merge_people(args: {
    from: Person;
    into: Person;
  }): Promise<void>;
  /** Runs `merge_people` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  merge_peopleMany(items: MergePeopleArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes one photo off a person, and the person off that photo. */
  split_photo(args: {
    person: Person;
    photo: string;
  }): Promise<void>;
  /** Runs `split_photo` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  split_photoMany(items: SplitPhotoArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Forgets a person: their name leaves every photo they were on. */
  remove_person_resource(args: {
    person: Person;
  }): Promise<void>;
  /** Runs `remove_person_resource` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_person_resourceMany(items: RemovePersonResourceArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Library` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LIBRARY_ACTIONS = ["import_photo", "absorb_picture", "import_photo_meta", "import_folder", "import_folderMany", "start_import", "start_importMany", "imported", "importedMany", "stop_import", "retry_import", "retry_importMany", "reindex", "reindexMany", "dismiss_import", "dismiss_importMany", "favorite_selected", "favorite_selectedMany", "remove_selected", "remove_selectedMany", "edit_selected", "edit_selectedMany", "new_album", "remove_album", "remove_albumMany", "start_album_draft", "start_album_draftMany", "album_draft_toggle", "album_draft_toggleMany", "cancel_album_draft", "cancel_album_draftMany", "save_album", "save_albumMany", "delete_album_draft", "delete_album_draftMany", "dismiss_notice", "dismiss_noticeMany", "add_person", "add_personMany", "remove_person", "remove_personMany", "rename_person", "rename_personMany", "face_scan", "set_recognition", "set_recognitionMany", "scan_faces", "scan_step", "read_batch", "pause_scan", "pause_scanMany", "delete_face_data", "delete_face_dataMany", "approve_suggestion", "approve_suggestionMany", "merge_suggestion", "merge_suggestionMany", "dismiss_suggestion", "dismiss_suggestionMany", "person_named", "new_person", "merge_people", "merge_peopleMany", "split_photo", "split_photoMany", "remove_person_resource", "remove_person_resourceMany"] as const;

/** The `tl.so.tilo.photos.Library` kind at `path`. */
export function library(ws: tilo.Workspace, path: string): Library {
  return resourceProxy(ws, path, SURFACE, LIBRARY_ACTIONS) as unknown as Library;
}

/** AlbumDraft — `tl.so.tilo.photos.AlbumDraft`. */
export interface AlbumDraft extends tilo.Resource {
  "53utp7nd": string;  // album
  jaabesu7: string;  // name
  l4infwly: string[];  // photos
}

/** The actions `AlbumDraft` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ALBUMDRAFT_ACTIONS = [] as const;

/** The `tl.so.tilo.photos.AlbumDraft` kind at `path`. */
export function albumDraft(ws: tilo.Workspace, path: string): AlbumDraft {
  return resourceProxy(ws, path, SURFACE, ALBUMDRAFT_ACTIONS) as unknown as AlbumDraft;
}

/** Unread — `tl.so.tilo.photos.Unread`. */
export interface Unread extends tilo.Resource {
  "6iu5enh3": string;  // name
  cpplykgh: string;  // picture
  "3iejvl3x": string;  // path
  vzrzob3d: string;  // why
  dof27753: boolean;  // retry
}

/** The actions `Unread` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const UNREAD_ACTIONS = [] as const;

/** The `tl.so.tilo.photos.Unread` kind at `path`. */
export function unread(ws: tilo.Workspace, path: string): Unread {
  return resourceProxy(ws, path, SURFACE, UNREAD_ACTIONS) as unknown as Unread;
}

/** ChosenPicture — `tl.so.tilo.photos.ChosenPicture`. */
export interface ChosenPicture extends tilo.Resource {
  wjvddrc5: string;  // name
  "6agrr5bt": string;  // url
  e67vj6hs: string;  // path
}

/** The actions `ChosenPicture` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CHOSENPICTURE_ACTIONS = [] as const;

/** The `tl.so.tilo.photos.ChosenPicture` kind at `path`. */
export function chosenPicture(ws: tilo.Workspace, path: string): ChosenPicture {
  return resourceProxy(ws, path, SURFACE, CHOSENPICTURE_ACTIONS) as unknown as ChosenPicture;
}

/** ImportAsk — `tl.so.tilo.photos.ImportAsk`. */
export interface ImportAsk extends tilo.Resource {
  pgrmuslm: ChosenPicture[];  // files
}

/** The actions `ImportAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const IMPORTASK_ACTIONS = [] as const;

/** The `tl.so.tilo.photos.ImportAsk` kind at `path`. */
export function importAsk(ws: tilo.Workspace, path: string): ImportAsk {
  return resourceProxy(ws, path, SURFACE, IMPORTASK_ACTIONS) as unknown as ImportAsk;
}

/** ReadPicture — `tl.so.tilo.photos.ReadPicture`. */
export interface ReadPicture extends tilo.Resource {
  z7dxm5rp: string;  // name
  bedul3kq: string;  // picture
  v72wrl73: string;  // thumb
  f56g7pyb: string;  // folder
  sc6xvdzp: string;  // captured_at
  ohsouzrl: number;  // latitude
  ug6uhwwb: number;  // longitude
}

/** The actions `ReadPicture` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const READPICTURE_ACTIONS = [] as const;

/** The `tl.so.tilo.photos.ReadPicture` kind at `path`. */
export function readPicture(ws: tilo.Workspace, path: string): ReadPicture {
  return resourceProxy(ws, path, SURFACE, READPICTURE_ACTIONS) as unknown as ReadPicture;
}

/** ImportRecord — `tl.so.tilo.photos.ImportRecord`. */
export interface ImportRecord extends tilo.Resource {
  k33gw4xu: string;  // job
  hbuvlsle: string;  // state
  l5ism53v: number;  // found
  nbqjpo64: number;  // done
  jrp3muui: ReadPicture[];  // photos
  affxqykm: Unread[];  // issues
}

/** The actions `ImportRecord` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const IMPORTRECORD_ACTIONS = [] as const;

/** The `tl.so.tilo.photos.ImportRecord` kind at `path`. */
export function importRecord(ws: tilo.Workspace, path: string): ImportRecord {
  return resourceProxy(ws, path, SURFACE, IMPORTRECORD_ACTIONS) as unknown as ImportRecord;
}

/** FaceScan — `tl.so.tilo.photos.FaceScan`. */
export interface FaceScan extends tilo.Resource {
  dejwr76b: boolean;  // on
  dcarwzkc: string;  // state
  quetfngb: string[];  // scanned
}

/** The actions `FaceScan` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FACESCAN_ACTIONS = [] as const;

/** The `tl.so.tilo.photos.FaceScan` kind at `path`. */
export function faceScan(ws: tilo.Workspace, path: string): FaceScan {
  return resourceProxy(ws, path, SURFACE, FACESCAN_ACTIONS) as unknown as FaceScan;
}

/** Photos — `tl.so.tilo.photos`. */
export interface Photos extends tilo.Resource {
}

/** The actions `Photos` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PHOTOS_ACTIONS = [] as const;

/** The `tl.so.tilo.photos` kind at `path`. */
export function photos(ws: tilo.Workspace, path: string): Photos {
  return resourceProxy(ws, path, SURFACE, PHOTOS_ACTIONS) as unknown as Photos;
}

