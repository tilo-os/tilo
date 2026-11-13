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
export const SURFACE = "so.tilo.hex-editor@7d5dcf386447ee8e";

/** Hex document — `hex-editor.document`. */
export interface Document extends tilo.Resource {
  readonly name: unknown;
  readonly opened: unknown;
  bytes: unknown;
  // the app's `load` action is the SDK's own: run it with `action("load")`.
  insertBytes(args: {
    offset: number;
    bytes: string;
  }): Promise<tilo.Run | void>;
  overwriteBytes(args: {
    offset: number;
    bytes: string;
  }): Promise<tilo.Run | void>;
  deleteBytes(args: {
    offset: number;
    len: number;
  }): Promise<tilo.Run | void>;
}

/** The `hex-editor.document` kind at `path`. */
export function hexEditorDocument(ws: tilo.Workspace, path: string): Document {
  return resourceProxy(ws, path, SURFACE) as unknown as Document;
}

