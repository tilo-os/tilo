/**
 * Types for Hex Editor (`so.tilo.hex-editor`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Document` is a `Resource` of the app's `hex-editor.document`, made by its factory with the
 * resource's path (`hex-editor/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.hex-editor@e3d7d95f6b291955";

/** The arguments of `insert-bytes`. */
export interface InsertBytesArgs {
  offset: number;
  bytes: string;
}

/** The arguments of `overwrite-bytes`. */
export interface OverwriteBytesArgs {
  offset: number;
  bytes: string;
}

/** The arguments of `delete-bytes`. */
export interface DeleteBytesArgs {
  offset: number;
  len: number;
}

/** Hex document — `hex-editor.document`. */
export interface Document extends tilo.Resource {
  readonly name: string | null;
  readonly opened: number;
  bytes: unknown;
  // the app's `load` action is the SDK's own: run it with `action("load")`.
  insertBytes(args: {
    offset: number;
    bytes: string;
  }): Promise<void>;
  /** Runs `insert-bytes` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  insertBytesMany(items: InsertBytesArgs[]): Promise<tilo.ActionAnswer[]>;
  overwriteBytes(args: {
    offset: number;
    bytes: string;
  }): Promise<void>;
  /** Runs `overwrite-bytes` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  overwriteBytesMany(items: OverwriteBytesArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteBytes(args: {
    offset: number;
    len: number;
  }): Promise<void>;
  /** Runs `delete-bytes` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteBytesMany(items: DeleteBytesArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Document` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DOCUMENT_ACTIONS = ["insertBytes", "insertBytesMany", "overwriteBytes", "overwriteBytesMany", "deleteBytes", "deleteBytesMany"] as const;

/** The `hex-editor.document` kind at `path`. */
export function hexEditorDocument(ws: tilo.Workspace, path: string): Document {
  return resourceProxy(ws, path, SURFACE, DOCUMENT_ACTIONS) as unknown as Document;
}

