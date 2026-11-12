/**
 * Types for Typst Editor (`so.tilo.typst-editor`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Workspace` is a `Resource` of the app's `typst-editor.workspace`, made by its factory with the
 * resource's path (`typst-editor/<resource>`): its attributes are the app's fields
 * (read-only ones `readonly`) and its methods are the app's actions, named in camel
 * case and taking the arguments the wire takes them under. An action that takes
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "@tilo/sdk";
import type * as tilo from "@tilo/sdk";

/** Typst workspace — `typst-editor.workspace`. */
export interface Workspace extends tilo.Resource {
  files: unknown;
  newFile(args: {
    name: string;
    path?: string;
    source?: string;
  }): Promise<tilo.Run | void>;
  savedAt(args: {
    file: string;
    path: string;
  }): Promise<tilo.Run | void>;
  // the app's `load` action is the SDK's own: run it with `action("load")`.
  closeFile(args: {
    file: string;
  }): Promise<tilo.Run | void>;
}

/** The `typst-editor.workspace` kind at `path`. */
export function workspace(ws: tilo.Workspace, path: string): Workspace {
  return resourceProxy(ws, path) as unknown as Workspace;
}

