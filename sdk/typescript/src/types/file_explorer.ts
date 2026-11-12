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
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

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

/** One of "opened", "changed". */
export type RecentUse = "opened" | "changed";

/** Folder — `files.folder`. */
export interface Folder extends tilo.Resource {
  places: Bookmark[];
  addBookmark(args: {
    place: string;
    at?: number;
  }): Promise<tilo.Run | void>;
  removeBookmark(args: {
    place: string;
  }): Promise<tilo.Run | void>;
  newFolder(args: {
    device: string;
    parent: string;
    name: string;
  }): Promise<unknown | tilo.Run>;
  rename(args: {
    device: string;
    folder: string;
    entry: string;
    name: string;
  }): Promise<unknown | tilo.Run>;
  trash(args: {
    device: string;
    folder: string;
    entries: string[];
  }): Promise<unknown | tilo.Run>;
  move(args: {
    device: string;
    from: string;
    entries: string[];
    to: string;
    create?: boolean;
  }): Promise<unknown | tilo.Run>;
  copyInto(args: {
    device: string;
    from: string;
    entries: string[];
    to: string;
  }): Promise<unknown | tilo.Run>;
  importMounted(args: {
    device: string;
    paths: string[];
    folder: string;
  }): Promise<unknown | tilo.Run>;
  cloudImport(args: {
    device: string;
    job_id: string;
    account: string;
    entries: unknown[];
    destination: unknown;
  }): Promise<unknown | tilo.Run>;
  writeFile(args: {
    device: string;
    folder: string;
    name: string;
    text: string;
    replace?: boolean;
  }): Promise<unknown | tilo.Run>;
  compress(args: {
    device: string;
    from: string;
    entries: string[];
    name?: string;
    format?: string;
  }): Promise<unknown | tilo.Run>;
  extract(args: {
    device: string;
    archive: string;
    to?: unknown;
    if_exists?: string;
  }): Promise<unknown | tilo.Run>;
  recordClipboardTransfer(args: {
    device: string;
    receipt: Receipt;
  }): Promise<unknown | tilo.Run>;
  recordTrashRestore(args: {
    device: string;
    receipt: Receipt;
  }): Promise<unknown | tilo.Run>;
  revert(args: {
    op: string;
  }): Promise<unknown | tilo.Run>;
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

/** The `files.folder` kind at `path`. */
export function folder(ws: tilo.Workspace, path: string): Folder {
  return resourceProxy(ws, path) as unknown as Folder;
}

/** Files — `files.tree`. */
export interface Tree extends tilo.Resource {
}

/** The `files.tree` kind at `path`. */
export function tree(ws: tilo.Workspace, path: string): Tree {
  return resourceProxy(ws, path) as unknown as Tree;
}

/** Recent files — `files.recent`. */
export interface Recent extends tilo.Resource {
  devices: Record<string, unknown>;
  note(args: {
    device: string;
    path: string;
    last_use: RecentUse;
    at_ms: number;
  }): Promise<tilo.Run | void>;
  moved(args: {
    device: string;
    from: string;
    to: string;
  }): Promise<tilo.Run | void>;
  forget(args: {
    device: string;
    path: string;
  }): Promise<tilo.Run | void>;
}

/** The `files.recent` kind at `path`. */
export function recent(ws: tilo.Workspace, path: string): Recent {
  return resourceProxy(ws, path) as unknown as Recent;
}

