/**
 * Types for Photos (`so.tilo.photos`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Library` is a `Resource` of the app's `photos.library`, made by its factory with the
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
export const SURFACE = "so.tilo.photos@733ec0557fdbf49c";

export interface Album {
  id: string;
  name: string;
  photo_ids: string[];
}

export interface AnalysisProvenance {
  source: string;
  engine: string;
  model_id: string;
  model_revision: string;
  language: string;
  content: string;
  scope: string;
}

export interface Edits {
  crop: unknown;
  rotate_quarters: number;
}

export interface ImageAnalysis {
  ocr_text: string;
  description: string;
  objects: string[];
  pets: string[];
  provenance: AnalysisProvenance;
  ocr_provenance: AnalysisProvenance | null;
  vision_provenance: AnalysisProvenance | null;
}

export interface LibraryPhoto {
  id: string;
  name: string;
  image: OpenImage;
  favorite: boolean;
  folder: string | null;
  people: string[];
  tags: string[];
  captured_at: string | null;
  location: unknown | null;
  ocr: unknown | null;
  vision: unknown | null;
  edits: Edits;
}

export interface LocationProvenance {
  source: string;
  scope: string;
}

export interface OpenImage {
  name: string;
  width: number;
  height: number;
  content: unknown;
}

export interface PhotoLocation {
  latitude: number;
  longitude: number;
  label: string | null;
  provenance: LocationProvenance;
}

export interface Rect01 {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** The arguments of `import-photo`. */
export interface ImportPhotoArgs {
  image: OpenImage;
  captured_at?: string;
  location?: PhotoLocation;
}

/** The arguments of `refresh-imported-metadata`. */
export interface RefreshImportedMetadataArgs {
  photo: string;
  captured_at?: string;
  location?: PhotoLocation;
}

/** The arguments of `remove-photo`. */
export interface RemovePhotoArgs {
  photo: string;
}

/** The arguments of `rotate-right`. */
export interface RotateRightArgs {
  photo: string;
}

/** The arguments of `rotate-left`. */
export interface RotateLeftArgs {
  photo: string;
}

/** The arguments of `crop`. */
export interface CropArgs {
  photo: string;
  region: Rect01;
}

/** The arguments of `merge-image-analysis`. */
export interface MergeImageAnalysisArgs {
  photo: string;
  analysis: ImageAnalysis;
}

/** The arguments of `set-person-membership`. */
export interface SetPersonMembershipArgs {
  person: string;
  photos: string[];
}

/** The arguments of `rename-person`. */
export interface RenamePersonArgs {
  person: string;
  to: string;
}

/** The arguments of `remove-person`. */
export interface RemovePersonArgs {
  person: string;
}

/** The arguments of `remove-album`. */
export interface RemoveAlbumArgs {
  album: string;
}

/** Library — `photos.library`. */
export interface Library extends tilo.Resource {
  photos: Array<tilo.Entry<LibraryPhoto>>;
  albums: Array<tilo.Entry<Album>>;
  importPhoto(args: {
    image: OpenImage;
    captured_at?: string;
    location?: PhotoLocation;
  }): Promise<void>;
  /** Runs `import-photo` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  importPhotoMany(items: ImportPhotoArgs[]): Promise<tilo.ActionAnswer[]>;
  refreshImportedMetadata(args: {
    photo: string;
    captured_at?: string;
    location?: PhotoLocation;
  }): Promise<void>;
  /** Runs `refresh-imported-metadata` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  refreshImportedMetadataMany(items: RefreshImportedMetadataArgs[]): Promise<tilo.ActionAnswer[]>;
  removePhoto(args: {
    photo: string;
  }): Promise<void>;
  /** Runs `remove-photo` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  removePhotoMany(items: RemovePhotoArgs[]): Promise<tilo.ActionAnswer[]>;
  rotateRight(args: {
    photo: string;
  }): Promise<void>;
  /** Runs `rotate-right` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rotateRightMany(items: RotateRightArgs[]): Promise<tilo.ActionAnswer[]>;
  rotateLeft(args: {
    photo: string;
  }): Promise<void>;
  /** Runs `rotate-left` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rotateLeftMany(items: RotateLeftArgs[]): Promise<tilo.ActionAnswer[]>;
  crop(args: {
    photo: string;
    region: Rect01;
  }): Promise<void>;
  /** Runs `crop` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  cropMany(items: CropArgs[]): Promise<tilo.ActionAnswer[]>;
  mergeImageAnalysis(args: {
    photo: string;
    analysis: ImageAnalysis;
  }): Promise<void>;
  /** Runs `merge-image-analysis` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  mergeImageAnalysisMany(items: MergeImageAnalysisArgs[]): Promise<tilo.ActionAnswer[]>;
  setPersonMembership(args: {
    person: string;
    photos: string[];
  }): Promise<void>;
  /** Runs `set-person-membership` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setPersonMembershipMany(items: SetPersonMembershipArgs[]): Promise<tilo.ActionAnswer[]>;
  renamePerson(args: {
    person: string;
    to: string;
  }): Promise<void>;
  /** Runs `rename-person` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  renamePersonMany(items: RenamePersonArgs[]): Promise<tilo.ActionAnswer[]>;
  removePerson(args: {
    person: string;
  }): Promise<void>;
  /** Runs `remove-person` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  removePersonMany(items: RemovePersonArgs[]): Promise<tilo.ActionAnswer[]>;
  removeAlbum(args: {
    album: string;
  }): Promise<void>;
  /** Runs `remove-album` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  removeAlbumMany(items: RemoveAlbumArgs[]): Promise<tilo.ActionAnswer[]>;
  makeThumbnails(args: {
    photos: unknown[];
  }): Promise<void>;
}

/** The actions `Library` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LIBRARY_ACTIONS = ["importPhoto", "importPhotoMany", "refreshImportedMetadata", "refreshImportedMetadataMany", "removePhoto", "removePhotoMany", "rotateRight", "rotateRightMany", "rotateLeft", "rotateLeftMany", "crop", "cropMany", "mergeImageAnalysis", "mergeImageAnalysisMany", "setPersonMembership", "setPersonMembershipMany", "renamePerson", "renamePersonMany", "removePerson", "removePersonMany", "removeAlbum", "removeAlbumMany", "makeThumbnails"] as const;

/** The `photos.library` kind at `path`. */
export function library(ws: tilo.Workspace, path: string): Library {
  return resourceProxy(ws, path, SURFACE, LIBRARY_ACTIONS) as unknown as Library;
}

