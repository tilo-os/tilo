/**
 * Types for Snippets (`so.tilo.snippets`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Library` is a `Resource` of the app's `snippets.library`, made by its factory with the
 * resource's path (`snippets/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.snippets@ab6662b1374b4cde";

/** One of "PlainText", "Shell", "Rust", "Sql", "Git", "Python", "JavaScript", "Json", "Yaml", "Markdown", "Html", "Css", "Toml", "Dockerfile". */
export type Language = "PlainText" | "Shell" | "Rust" | "Sql" | "Git" | "Python" | "JavaScript" | "Json" | "Yaml" | "Markdown" | "Html" | "Css" | "Toml" | "Dockerfile";

export interface Snippet {
  title: string;
  body: string;
  description: string;
  favorite: boolean;
  folder: string;
  language: Language;
  tags: string[];
  uses: number;
}

/** The arguments of `new-snippet`. */
export interface NewSnippetArgs {
  title: string;
  folder?: string;
}

/** The arguments of `duplicate`. */
export interface DuplicateArgs {
  snippet: string;
  title?: string;
}

/** The arguments of `delete`. */
export interface DeleteArgs {
  snippet: string;
}

/** The arguments of `record-use`. */
export interface RecordUseArgs {
  snippet: string;
}

/** The arguments of `rename-folder`. */
export interface RenameFolderArgs {
  folder: string;
  to: string;
}

/** The arguments of `rename-tag`. */
export interface RenameTagArgs {
  tag: string;
  to: string;
}

/** The arguments of `remove-tag`. */
export interface RemoveTagArgs {
  tag: string;
}

/** Snippet library — `snippets.library`. */
export interface Library extends tilo.Resource {
  snippets: Array<tilo.Entry<Snippet>>;
  newSnippet(args: {
    title: string;
    folder?: string;
  }): Promise<void>;
  /** Runs `new-snippet` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  newSnippetMany(items: NewSnippetArgs[]): Promise<tilo.ActionAnswer[]>;
  duplicate(args: {
    snippet: string;
    title?: string;
  }): Promise<void>;
  /** Runs `duplicate` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  duplicateMany(items: DuplicateArgs[]): Promise<tilo.ActionAnswer[]>;
  delete(args: {
    snippet: string;
  }): Promise<void>;
  /** Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  deleteMany(items: DeleteArgs[]): Promise<tilo.ActionAnswer[]>;
  recordUse(args: {
    snippet: string;
  }): Promise<void>;
  /** Runs `record-use` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  recordUseMany(items: RecordUseArgs[]): Promise<tilo.ActionAnswer[]>;
  renameFolder(args: {
    folder: string;
    to: string;
  }): Promise<void>;
  /** Runs `rename-folder` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  renameFolderMany(items: RenameFolderArgs[]): Promise<tilo.ActionAnswer[]>;
  renameTag(args: {
    tag: string;
    to: string;
  }): Promise<void>;
  /** Runs `rename-tag` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  renameTagMany(items: RenameTagArgs[]): Promise<tilo.ActionAnswer[]>;
  removeTag(args: {
    tag: string;
  }): Promise<void>;
  /** Runs `remove-tag` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  removeTagMany(items: RemoveTagArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Library` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LIBRARY_ACTIONS = ["newSnippet", "newSnippetMany", "duplicate", "duplicateMany", "delete", "deleteMany", "recordUse", "recordUseMany", "renameFolder", "renameFolderMany", "renameTag", "renameTagMany", "removeTag", "removeTagMany"] as const;

/** The `snippets.library` kind at `path`. */
export function library(ws: tilo.Workspace, path: string): Library {
  return resourceProxy(ws, path, SURFACE, LIBRARY_ACTIONS) as unknown as Library;
}

