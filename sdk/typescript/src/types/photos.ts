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
export const SURFACE = "so.tilo.photos@f9b8a942b600bf8f";

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

/** Library — `photos.library`. */
export interface Library extends tilo.Resource {
  photos: LibraryPhoto[];
  albums: Album[];
  importPhoto(args: {
    image: OpenImage;
    captured_at?: string;
    location?: PhotoLocation;
  }): Promise<tilo.Run | void>;
  refreshImportedMetadata(args: {
    photo: string;
    captured_at?: string;
    location?: PhotoLocation;
  }): Promise<tilo.Run | void>;
  removePhoto(args: {
    photo: string;
  }): Promise<tilo.Run | void>;
  rotateRight(args: {
    photo: string;
  }): Promise<tilo.Run | void>;
  rotateLeft(args: {
    photo: string;
  }): Promise<tilo.Run | void>;
  crop(args: {
    photo: string;
    region: Rect01;
  }): Promise<tilo.Run | void>;
  mergeImageAnalysis(args: {
    photo: string;
    analysis: ImageAnalysis;
  }): Promise<tilo.Run | void>;
  setPersonMembership(args: {
    person: string;
    photos: string[];
  }): Promise<tilo.Run | void>;
  renamePerson(args: {
    person: string;
    to: string;
  }): Promise<tilo.Run | void>;
  removePerson(args: {
    person: string;
  }): Promise<tilo.Run | void>;
  removeAlbum(args: {
    album: string;
  }): Promise<tilo.Run | void>;
  makeThumbnails(args: {
    photos: unknown[];
  }): Promise<void>;
}

/** The `photos.library` kind at `path`. */
export function library(ws: tilo.Workspace, path: string): Library {
  return resourceProxy(ws, path, SURFACE) as unknown as Library;
}

