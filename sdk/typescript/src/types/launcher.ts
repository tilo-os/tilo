/**
 * Types for Search (`so.tilo.launcher`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Search` is a `Resource` of the app's `launcher.search`, made by its factory with the
 * resource's path (`launcher/<resource>`): its attributes are the app's fields
 * (read-only ones `readonly`) and its methods are the app's actions, named in camel
 * case and taking the arguments the wire takes them under. An action that takes
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** Search — `launcher.search`. */
export interface Search extends tilo.Resource {
}

/** The `launcher.search` kind at `path`. */
export function search(ws: tilo.Workspace, path: string): Search {
  return resourceProxy(ws, path) as unknown as Search;
}

