/**
 * Types for Photos (`so.tilo.photos`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Photo` is a `Resource` of the app's `tl.so.tilo.photos.Photo`, made by its factory with the
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
export const SURFACE = "so.tilo.photos@96e9dbabec8e793f";

/** What an edit does to one field of every selected photo: keeps what each photo has, takes it off all of them, gives them all one value, or adds or takes off names (tags, people). One of "keep", "clear", "set", "add", "remove". */
export type Change = "keep" | "clear" | "set" | "add" | "remove";

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

/** The arguments of `import_dropped`. */
export interface ImportDroppedArgs {
  files: unknown[];
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

/** Photo — `tl.so.tilo.photos.Photo`. */
export interface Photo extends tilo.Resource {
  rv4vs7su: string;  // name
  r55t3rg3: string;  // picture
  vwrtdas3: string;  // thumb
  "2i7i3as5": string;  // edited
  ktv6ci4q: number;  // rotation
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
  /** Marks a photo as a favorite, or takes that back. */
  toggle_favorite(args?: {
    favorite?: boolean;
  }): Promise<void>;
  /** Runs `toggle_favorite` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  toggle_favoriteMany(items: ToggleFavoriteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Turns the photograph a quarter clockwise, as its person sees it: the window draws the original turned, and a save writes what it draws. */
  rotate_right(): Promise<void>;
  /** Runs `rotate_right` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rotate_rightMany(items: RotateRightArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Turns the photograph a quarter anticlockwise. */
  rotate_left(): Promise<void>;
  /** Runs `rotate_left` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rotate_leftMany(items: RotateLeftArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts the photograph back as it came, taking the person's own turns off it. */
  reset_edits(): Promise<void>;
  /** Runs `reset_edits` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  reset_editsMany(items: ResetEditsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** What a picture shows, in a sentence, as the device's image model reads it: the sentence the library's details show and search finds. The work is the device's own, asked of this session on the app's behalf, so the app never runs a model itself. */
  describe_photo(): Promise<void>;
  /** The words in the photograph, read on this device and kept with the photo, so the library finds it by what it says. Needs no model: a device without one still reads its text. */
  recognize_text(): Promise<void>;
  /** Runs `recognize_text` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  recognize_textMany(items: RecognizeTextArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Copies the photograph to the person's clipboard as a picture, so it pastes into another app. */
  copy_photo(): Promise<void>;
  /** The objects and pets the device's image model finds in a picture, added to what the photo already lists. */
  tag_photo(): Promise<void>;
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
export const PHOTO_ACTIONS = ["toggle_favorite", "toggle_favoriteMany", "rotate_right", "rotate_rightMany", "rotate_left", "rotate_leftMany", "reset_edits", "reset_editsMany", "describe_photo", "recognize_text", "recognize_textMany", "copy_photo", "tag_photo", "crop_photo", "crop_photoMany", "tag", "tagMany", "untag", "untagMany", "share", "set_place", "set_placeMany", "clear_place", "clear_placeMany", "export_photo"] as const;

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
  /** Imports the pictures a person drops on the window or picks a folder of: each picture file becomes a photo, with the folder it came in, and a file that is no picture is left alone and said so in the report. Picking the same folder again brings what the files say to the photos already there, rather than making a second copy of each. */
  import_dropped(args: {
    files: unknown[];
  }): Promise<void>;
  /** Runs `import_dropped` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  import_droppedMany(items: ImportDroppedArgs[]): Promise<tilo.ActionAnswer[]>;
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
  /** The faces the device finds in `photos`, kept as suggestions of the person's own: a face has no name, so nothing of it reaches another member until a name is approved. */
  find_faces(args: {
    photos: Photo[];
  }): Promise<void>;
  /** Scans the next photos on this device nobody is named on yet: the model takes twelve at a time, so a scan is one batch, and each face it finds is a suggestion of the scanner's own. */
  scan_faces(): Promise<void>;
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
export const LIBRARY_ACTIONS = ["import_photo", "absorb_picture", "import_photo_meta", "import_dropped", "import_droppedMany", "dismiss_import", "dismiss_importMany", "favorite_selected", "favorite_selectedMany", "remove_selected", "remove_selectedMany", "edit_selected", "edit_selectedMany", "new_album", "remove_album", "remove_albumMany", "add_person", "add_personMany", "remove_person", "remove_personMany", "rename_person", "rename_personMany", "find_faces", "scan_faces", "approve_suggestion", "approve_suggestionMany", "merge_suggestion", "merge_suggestionMany", "dismiss_suggestion", "dismiss_suggestionMany", "person_named", "new_person", "merge_people", "merge_peopleMany", "split_photo", "split_photoMany", "remove_person_resource", "remove_person_resourceMany"] as const;

/** The `tl.so.tilo.photos.Library` kind at `path`. */
export function library(ws: tilo.Workspace, path: string): Library {
  return resourceProxy(ws, path, SURFACE, LIBRARY_ACTIONS) as unknown as Library;
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

