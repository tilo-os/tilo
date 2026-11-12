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
 * says where it is.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** Snippets — `snippets.library`. */
export interface Library extends tilo.Resource {
  snippets: unknown;
  newSnippet(args: {
    title: string;
    folder?: string;
  }): Promise<tilo.Run | void>;
  duplicate(args: {
    snippet: string;
    title?: string;
  }): Promise<tilo.Run | void>;
  delete(args: {
    snippet: string;
  }): Promise<tilo.Run | void>;
  recordUse(args: {
    snippet: string;
  }): Promise<tilo.Run | void>;
  renameFolder(args: {
    folder: string;
    to: string;
  }): Promise<tilo.Run | void>;
  renameTag(args: {
    tag: string;
    to: string;
  }): Promise<tilo.Run | void>;
  removeTag(args: {
    tag: string;
  }): Promise<tilo.Run | void>;
}

/** The `snippets.library` kind at `path`. */
export function library(ws: tilo.Workspace, path: string): Library {
  return resourceProxy(ws, path) as unknown as Library;
}

