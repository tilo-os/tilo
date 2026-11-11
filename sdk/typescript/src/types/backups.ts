/**
 * Types for Backups (`so.tilo.backups`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Workspace` is a `Resource` of the app's `backups.workspace`, made by its factory with the
 * resource's path (`backups/<resource>`): its attributes are the app's fields
 * (read-only ones `readonly`) and its methods are the app's actions, named in camel
 * case and taking the arguments the wire takes them under. An action that takes
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

export interface RetentionPolicy {
  hourly: number;
  daily: number;
  weekly: number;
  monthly: number;
}

/** Backups — `backups.workspace`. */
export interface Workspace extends tilo.Resource {
  policy: RetentionPolicy;
  verify(args: {
    snapshot_id: string;
  }): Promise<void>;
}

/** The `backups.workspace` kind at `path`. */
export function workspace(ws: tilo.Workspace, path: string): Workspace {
  return resourceProxy(ws, path) as unknown as Workspace;
}

