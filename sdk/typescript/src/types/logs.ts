/**
 * Types for Logs (`so.tilo.logs`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Journal` is a `Resource` of the app's `logs.journal`, made by its factory with the
 * resource's path (`logs/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.logs@2428af2372c28388";

/** One of "emerg", "alert", "crit", "err", "warning", "notice", "info", "debug". */
export type Priority = "emerg" | "alert" | "crit" | "err" | "warning" | "notice" | "info" | "debug";

/** System journal — `logs.journal`. */
export interface Journal extends tilo.Resource {
  severity: Priority;
  unit: string;
  boot: string;
  search: string;
  clearFilters(): Promise<tilo.Run | void>;
  // the app's `refresh` action is the SDK's own: run it with `action("refresh")`.
  setLiveTail(args?: {
    enabled?: boolean;
  }): Promise<void>;
  selectEntry(args: {
    entry_id: string;
  }): Promise<void>;
}

/** The `logs.journal` kind at `path`. */
export function journal(ws: tilo.Workspace, path: string): Journal {
  return resourceProxy(ws, path, SURFACE) as unknown as Journal;
}

