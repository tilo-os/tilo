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
 * says where it is. An undoable action that answers nothing also has a `<action>Many`,
which applies it to every argument object of a list in a few all-or-nothing steps,
all of them one run.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** The version of the surface these types were made from: a workspace whose app
 * publishes another refuses their actions rather than run what they no longer
 * describe. */
export const SURFACE = "so.tilo.ebook@ae688919837912da";

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

/** The arguments of `shelve`. */
export interface ShelveArgs {
  book: string;
  title: string;
  author: string;
  subject: string;
  shared: boolean;
  content?: unknown;
}

/** The arguments of `shelve-found`. */
export interface ShelveFoundArgs {
  book: string;
  title: string;
  author: string;
  subject: string;
}

/** The arguments of `remove-from-library`. */
export interface RemoveFromLibraryArgs {
  book: string;
}

/** The arguments of `index-book`. */
export interface IndexBookArgs {
  book: string;
  index: unknown;
}

/** The arguments of `record-progress`. */
export interface RecordProgressArgs {
  book: string;
  chapter: number;
  block: number;
  fraction: number;
  opened: boolean;
}

/** The arguments of `add-note`. */
export interface AddNoteArgs {
  passage: Passage;
  color: string;
  text: string;
}

/** The arguments of `add-comment`. */
export interface AddCommentArgs {
  highlight: string;
  text: string;
}

/** The arguments of `toggle-bookmark`. */
export interface ToggleBookmarkArgs {
  book: string;
  chapter: number;
  block: number;
  excerpt: string;
}

/** The arguments of `highlight`. */
export interface HighlightArgs {
  book: string;
  chapter: number;
  color: string;
  replacing: string[];
  pieces: HiPiece[];
}

/** The arguments of `change-color`. */
export interface ChangeColorArgs {
  highlight: string;
  color: string;
}

/** The arguments of `delete-highlight`. */
export interface DeleteHighlightArgs {
  highlight: string;
}

/** The arguments of `delete-annotation`. */
export interface DeleteAnnotationArgs {
  annotation: string;
}

/** The arguments of `remove-font`. */
export interface RemoveFontArgs {
  font: string;
}

/** The arguments of `delete-scheme`. */
export interface DeleteSchemeArgs {
  scheme: string;
}

/** The arguments of `keep`. */
export interface KeepArgs {
  device: string;
  kept: unknown;
}

/** Books — `ebook.library`. */
export interface Library extends tilo.Resource {
  entries: Array<tilo.Entry<LibraryEntry>>;
  progress: Record<string, Progress>;
  settings: Settings;
  default_font: string;
  prefs: LibraryPrefs;
  fonts: Array<tilo.Entry<FontEntry>>;
  schemes: Array<tilo.Entry<ColorScheme>>;
  annotations: Array<tilo.Entry<Mark>>;
  shelve(args: {
    book: string;
    title: string;
    author: string;
    subject: string;
    shared: boolean;
    content?: unknown;
  }): Promise<void>;
  /** Runs `shelve` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  shelveMany(items: ShelveArgs[]): Promise<tilo.ActionAnswer[]>;
  shelveFound(args: {
    book: string;
    title: string;
    author: string;
    subject: string;
  }): Promise<void>;
  /** Runs `shelve-found` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  shelveFoundMany(items: ShelveFoundArgs[]): Promise<tilo.ActionAnswer[]>;
  removeFromLibrary(args: {
    book: string;
  }): Promise<void>;
  /** Runs `remove-from-library` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  removeFromLibraryMany(items: RemoveFromLibraryArgs[]): Promise<tilo.ActionAnswer[]>;
  indexBook(args: {
    book: string;
    index: unknown;
  }): Promise<void>;
  /** Runs `index-book` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  indexBookMany(items: IndexBookArgs[]): Promise<tilo.ActionAnswer[]>;
  recordProgress(args: {
    book: string;
    chapter: number;
    block: number;
    fraction: number;
    opened: boolean;
  }): Promise<void>;
  /** Runs `record-progress` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  recordProgressMany(items: RecordProgressArgs[]): Promise<tilo.ActionAnswer[]>;
  addNote(args: {
    passage: Passage;
    color: string;
    text: string;
  }): Promise<void>;
  /** Runs `add-note` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  addNoteMany(items: AddNoteArgs[]): Promise<tilo.ActionAnswer[]>;
  addComment(args: {
    highlight: string;
    text: string;
  }): Promise<void>;
  /** Runs `add-comment` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  addCommentMany(items: AddCommentArgs[]): Promise<tilo.ActionAnswer[]>;
  toggleBookmark(args: {
    book: string;
    chapter: number;
    block: number;
    excerpt: string;
  }): Promise<void>;
  /** Runs `toggle-bookmark` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  toggleBookmarkMany(items: ToggleBookmarkArgs[]): Promise<tilo.ActionAnswer[]>;
  highlight(args: {
    book: string;
    chapter: number;
    color: string;
    replacing: string[];
    pieces: HiPiece[];
  }): Promise<void>;
  /** Runs `highlight` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  highlightMany(items: HighlightArgs[]): Promise<tilo.ActionAnswer[]>;
  changeColor(args: {
    highlight: string;
    color: string;
  }): Promise<void>;
  /** Runs `change-color` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  changeColorMany(items: ChangeColorArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteHighlight(args: {
    highlight: string;
  }): Promise<void>;
  /** Runs `delete-highlight` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  deleteHighlightMany(items: DeleteHighlightArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteAnnotation(args: {
    annotation: string;
  }): Promise<void>;
  /** Runs `delete-annotation` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  deleteAnnotationMany(items: DeleteAnnotationArgs[]): Promise<tilo.ActionAnswer[]>;
  removeFont(args: {
    font: string;
  }): Promise<void>;
  /** Runs `remove-font` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  removeFontMany(items: RemoveFontArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteScheme(args: {
    scheme: string;
  }): Promise<void>;
  /** Runs `delete-scheme` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  deleteSchemeMany(items: DeleteSchemeArgs[]): Promise<tilo.ActionAnswer[]>;
  readChapter(args: {
    book: string;
    chapter?: number;
  }): Promise<void>;
}

/** The actions `Library` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LIBRARY_ACTIONS = ["shelve", "shelveMany", "shelveFound", "shelveFoundMany", "removeFromLibrary", "removeFromLibraryMany", "indexBook", "indexBookMany", "recordProgress", "recordProgressMany", "addNote", "addNoteMany", "addComment", "addCommentMany", "toggleBookmark", "toggleBookmarkMany", "highlight", "highlightMany", "changeColor", "changeColorMany", "deleteHighlight", "deleteHighlightMany", "deleteAnnotation", "deleteAnnotationMany", "removeFont", "removeFontMany", "deleteScheme", "deleteSchemeMany", "readChapter"] as const;

/** The `ebook.library` kind at `path`. */
export function library(ws: tilo.Workspace, path: string): Library {
  return resourceProxy(ws, path, SURFACE, LIBRARY_ACTIONS) as unknown as Library;
}

/** Book catalogues — `settings.books-catalogues`. */
export interface BooksCatalogues extends tilo.Resource {
  keep_catalogues: boolean;
}

/** The actions `BooksCatalogues` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const BOOKSCATALOGUES_ACTIONS = [] as const;

/** The `settings.books-catalogues` kind at `path`. */
export function booksCatalogues(ws: tilo.Workspace, path: string): BooksCatalogues {
  return resourceProxy(ws, path, SURFACE, BOOKSCATALOGUES_ACTIONS) as unknown as BooksCatalogues;
}

/** Book files — `ebook.files`. */
export interface Files extends tilo.Resource {
  devices: Record<string, unknown>;
  keep(args: {
    device: string;
    kept: unknown;
  }): Promise<void>;
  /** Runs `keep` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  keepMany(items: KeepArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Files` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FILES_ACTIONS = ["keep", "keepMany"] as const;

/** The `ebook.files` kind at `path`. */
export function files(ws: tilo.Workspace, path: string): Files {
  return resourceProxy(ws, path, SURFACE, FILES_ACTIONS) as unknown as Files;
}

