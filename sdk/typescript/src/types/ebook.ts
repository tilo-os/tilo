/**
 * Types for E-Book Reader (`so.tilo.ebook`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Library` is a `Resource` of the app's `ebook.library`, made by its factory with the
 * resource's path (`ebook/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.ebook@f7c27282fa766530";

/** One of "bookmark", "highlight", "note". */
export type AnnoKind = "bookmark" | "highlight" | "note";

export interface ColorScheme {
  id: string;
  name: string;
  page: number[];
  ink: number[];
  accent: number[];
  dark: boolean;
  enabled: boolean;
  builtin: boolean;
}

/** One of "book", "ragged". */
export type CompositionStyle = "book" | "ragged";

export interface FontEntry {
  id: string;
  kind: unknown;
  name: string;
  size_mult: number;
  shown: boolean;
}

export interface HiPiece {
  block: number;
  start: number;
  end: number;
  excerpt: string;
}

export interface LibraryEntry {
  id: string;
  shared: boolean;
  title: string;
  author: string;
  subject: string;
  added: number;
  content: unknown;
  index: unknown;
}

export interface LibraryPrefs {
  sort: LibrarySort;
  hide_read: boolean;
}

/** One of "title", "author", "recent", "progress". */
export type LibrarySort = "title" | "author" | "recent" | "progress";

export interface Mark {
  id: string;
  doc_id: string;
  kind: AnnoKind;
  chapter: number;
  block: number;
  start: number;
  end: number;
  excerpt: string;
  text: string;
  color: unknown;
  created: number;
  group: string;
}

export interface Passage {
  book: string;
  chapter: number;
  block: number;
  start: number;
  end: number;
  excerpt: string;
}

export interface Progress {
  chapter: number;
  block: number;
  fraction: number;
  opened: number;
}

/** One of "paged", "continuous", "two_up". */
export type ReadMode = "paged" | "continuous" | "two_up";

/** One of "light", "sepia", "dark", "high_contrast". */
export type ReadTheme = "light" | "sepia" | "dark" | "high_contrast";

export interface Settings {
  font_scale: number;
  line_spacing: number;
  font_id: string;
  theme: ReadTheme;
  scheme_id: string;
  mode: ReadMode;
  composition: CompositionStyle;
}

/** Books — `ebook.library`. */
export interface Library extends tilo.Resource {
  entries: LibraryEntry[];
  progress: Record<string, Progress>;
  settings: Settings;
  default_font: string;
  prefs: LibraryPrefs;
  fonts: FontEntry[];
  schemes: ColorScheme[];
  annotations: Mark[];
  shelve(args: {
    book: string;
    title: string;
    author: string;
    subject: string;
    shared: boolean;
    content?: unknown;
  }): Promise<tilo.Run | void>;
  shelveFound(args: {
    book: string;
    title: string;
    author: string;
    subject: string;
  }): Promise<tilo.Run | void>;
  removeFromLibrary(args: {
    book: string;
  }): Promise<tilo.Run | void>;
  indexBook(args: {
    book: string;
    index: unknown;
  }): Promise<tilo.Run | void>;
  recordProgress(args: {
    book: string;
    chapter: number;
    block: number;
    fraction: number;
    opened: boolean;
  }): Promise<tilo.Run | void>;
  addNote(args: {
    passage: Passage;
    color: string;
    text: string;
  }): Promise<tilo.Run | void>;
  addComment(args: {
    highlight: string;
    text: string;
  }): Promise<tilo.Run | void>;
  toggleBookmark(args: {
    book: string;
    chapter: number;
    block: number;
    excerpt: string;
  }): Promise<tilo.Run | void>;
  highlight(args: {
    book: string;
    chapter: number;
    color: string;
    replacing: string[];
    pieces: HiPiece[];
  }): Promise<tilo.Run | void>;
  changeColor(args: {
    highlight: string;
    color: string;
  }): Promise<tilo.Run | void>;
  deleteHighlight(args: {
    highlight: string;
  }): Promise<tilo.Run | void>;
  deleteAnnotation(args: {
    annotation: string;
  }): Promise<tilo.Run | void>;
  removeFont(args: {
    font: string;
  }): Promise<tilo.Run | void>;
  deleteScheme(args: {
    scheme: string;
  }): Promise<tilo.Run | void>;
  readChapter(args: {
    book: string;
    chapter?: number;
  }): Promise<void>;
}

/** The `ebook.library` kind at `path`. */
export function library(ws: tilo.Workspace, path: string): Library {
  return resourceProxy(ws, path, SURFACE) as unknown as Library;
}

/** Book catalogues — `settings.books-catalogues`. */
export interface BooksCatalogues extends tilo.Resource {
  keep_catalogues: boolean;
}

/** The `settings.books-catalogues` kind at `path`. */
export function booksCatalogues(ws: tilo.Workspace, path: string): BooksCatalogues {
  return resourceProxy(ws, path, SURFACE) as unknown as BooksCatalogues;
}

/** Book files — `ebook.files`. */
export interface Files extends tilo.Resource {
  devices: Record<string, unknown>;
  keep(args: {
    device: string;
    kept: unknown;
  }): Promise<tilo.Run | void>;
}

/** The `ebook.files` kind at `path`. */
export function files(ws: tilo.Workspace, path: string): Files {
  return resourceProxy(ws, path, SURFACE) as unknown as Files;
}

