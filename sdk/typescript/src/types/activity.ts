/**
 * Types for Activity (`so.tilo.activity`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Filter` is a `Resource` of the app's `activity.filter`, made by its factory with the
 * resource's path (`activity/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.activity@f7fe2955835947ba";

/** Workspace activity — `activity.filter`. */
export interface Filter extends tilo.Resource {
  period: string;
  author: string;
  lens: string;
  query: string;
  clearFilters(): Promise<tilo.Run | void>;
}

/** The `activity.filter` kind at `path`. */
export function filter(ws: tilo.Workspace, path: string): Filter {
  return resourceProxy(ws, path, SURFACE) as unknown as Filter;
}

