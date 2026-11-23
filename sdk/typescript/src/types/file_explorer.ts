/**
 * Types for Files (`so.tilo.file-explorer`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Folder` is a `Resource` of the app's `files.folder`, made by its factory with the
 * resource's path (`file-explorer/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.file-explorer@511cdc6be846faa8";

export interface Bookmark {
  place: string;
}

export interface FileTask {
  device: string;
  op: unknown;
}

export interface Receipt {
  operation: string;
  completed: number;
  cut: boolean;
  restore: boolean;
  error: string | null;
}

/** The arguments of `add-bookmark`. */
export interface AddBookmarkArgs {
  place: string;
  at?: number;
}

/** The arguments of `remove-bookmark`. */
export interface RemoveBookmarkArgs {
  place: string;
}

/** The arguments of `new-folder`. */
export interface NewFolderArgs {
  parent: unknown;
  name: unknown;
  document?: unknown;
}

/** The arguments of `new-file`. */
export interface NewFileArgs {
  parent: unknown;
  name: unknown;
  document?: unknown;
}

/** The arguments of `move`. */
export interface MoveArgs {
  node: unknown;
  parent: unknown;
  name: unknown;
}

/** The arguments of `write`. */
export interface WriteArgs {
  node: unknown;
  content: unknown;
  size: unknown;
}

/** The arguments of `trash`. */
export interface TrashArgs {
  node: unknown;
}

/** The arguments of `restore`. */
export interface RestoreArgs {
  node: unknown;
}

/** The arguments of `list-chunk`. */
export interface ListChunkArgs {
  chunk: unknown;
}

/** Folder — `files.folder`. */
export interface Folder extends tilo.Resource {
  places: Array<tilo.Entry<Bookmark>>;
  addBookmark(args: {
    place: string;
    at?: number;
  }): Promise<void>;
  /** Runs `add-bookmark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  addBookmarkMany(items: AddBookmarkArgs[]): Promise<tilo.ActionAnswer[]>;
  removeBookmark(args: {
    place: string;
  }): Promise<void>;
  /** Runs `remove-bookmark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  removeBookmarkMany(items: RemoveBookmarkArgs[]): Promise<tilo.ActionAnswer[]>;
  newFolder(args: {
    device: string;
    parent: string;
    name: string;
  }): Promise<unknown>;
  rename(args: {
    device: string;
    folder: string;
    entry: string;
    name: string;
  }): Promise<unknown>;
  trash(args: {
    device: string;
    folder: string;
    entries: string[];
  }): Promise<unknown>;
  move(args: {
    device: string;
    from: string;
    entries: string[];
    to: string;
    create?: boolean;
  }): Promise<unknown>;
  copyInto(args: {
    device: string;
    from: string;
    entries: string[];
    to: string;
  }): Promise<unknown>;
  importMounted(args: {
    device: string;
    paths: string[];
    folder: string;
  }): Promise<unknown>;
  cloudImport(args: {
    device: string;
    job_id: string;
    account: string;
    entries: unknown[];
    destination: unknown;
  }): Promise<unknown>;
  writeFile(args: {
    device: string;
    folder: string;
    name: string;
    text: string;
    replace?: boolean;
  }): Promise<unknown>;
  compress(args: {
    device: string;
    from: string;
    entries: string[];
    name?: string;
    format?: string;
  }): Promise<unknown>;
  extract(args: {
    device: string;
    archive: string;
    to?: unknown;
    if_exists?: string;
  }): Promise<unknown>;
  recordClipboardTransfer(args: {
    device: string;
    receipt: Receipt;
  }): Promise<unknown>;
  recordTrashRestore(args: {
    device: string;
    receipt: Receipt;
  }): Promise<unknown>;
  revert(args: {
    op: string;
  }): Promise<unknown>;
  // the app's `refresh` action is the SDK's own: run it with `action("refresh")`.
  handOver(args: {
    file: string;
  }): Promise<void>;
  readText(args: {
    file: string;
    from?: number;
  }): Promise<void>;
  copySelection(): Promise<void>;
  clipboardStatus(): Promise<void>;
  openTerminal(): Promise<void>;
  cloudProgress(): Promise<void>;
}

/** The actions `Folder` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FOLDER_ACTIONS = ["addBookmark", "addBookmarkMany", "removeBookmark", "removeBookmarkMany", "newFolder", "rename", "trash", "move", "copyInto", "importMounted", "cloudImport", "writeFile", "compress", "extract", "recordClipboardTransfer", "recordTrashRestore", "revert", "handOver", "readText", "copySelection", "clipboardStatus", "openTerminal", "cloudProgress"] as const;

/** The `files.folder` kind at `path`. */
export function folder(ws: tilo.Workspace, path: string): Folder {
  return resourceProxy(ws, path, SURFACE, FOLDER_ACTIONS) as unknown as Folder;
}

/** Files — `files.tree`. */
export interface Tree extends tilo.Resource {
  newFolder(args: {
    parent: unknown;
    name: unknown;
    document?: unknown;
  }): Promise<void>;
  /** Runs `new-folder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  newFolderMany(items: NewFolderArgs[]): Promise<tilo.ActionAnswer[]>;
  newFile(args: {
    parent: unknown;
    name: unknown;
    document?: unknown;
  }): Promise<void>;
  /** Runs `new-file` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  newFileMany(items: NewFileArgs[]): Promise<tilo.ActionAnswer[]>;
  move(args: {
    node: unknown;
    parent: unknown;
    name: unknown;
  }): Promise<void>;
  /** Runs `move` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  moveMany(items: MoveArgs[]): Promise<tilo.ActionAnswer[]>;
  write(args: {
    node: unknown;
    content: unknown;
    size: unknown;
  }): Promise<void>;
  /** Runs `write` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  writeMany(items: WriteArgs[]): Promise<tilo.ActionAnswer[]>;
  trash(args: {
    node: unknown;
  }): Promise<void>;
  /** Runs `trash` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  trashMany(items: TrashArgs[]): Promise<tilo.ActionAnswer[]>;
  restore(args: {
    node: unknown;
  }): Promise<void>;
  /** Runs `restore` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  restoreMany(items: RestoreArgs[]): Promise<tilo.ActionAnswer[]>;
  listChunk(args: {
    chunk: unknown;
  }): Promise<void>;
  /** Runs `list-chunk` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  listChunkMany(items: ListChunkArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Tree` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TREE_ACTIONS = ["newFolder", "newFolderMany", "newFile", "newFileMany", "move", "moveMany", "write", "writeMany", "trash", "trashMany", "restore", "restoreMany", "listChunk", "listChunkMany"] as const;

/** The `files.tree` kind at `path`. */
export function tree(ws: tilo.Workspace, path: string): Tree {
  return resourceProxy(ws, path, SURFACE, TREE_ACTIONS) as unknown as Tree;
}

/** Files window — `files.workspace`. */
export interface Workspace extends tilo.Resource {
  up(): Promise<void>;
  download(args: {
    node: string;
  }): Promise<void>;
}

/** The actions `Workspace` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WORKSPACE_ACTIONS = ["up", "download"] as const;

/** The `files.workspace` kind at `path`. */
export function workspace(ws: tilo.Workspace, path: string): Workspace {
  return resourceProxy(ws, path, SURFACE, WORKSPACE_ACTIONS) as unknown as Workspace;
}

