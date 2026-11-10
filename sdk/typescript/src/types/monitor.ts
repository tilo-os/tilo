/**
 * Types for System Monitor (`so.tilo.monitor`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Processes` is a `Resource` of the app's `monitor.processes`, made by its factory with the
 * resource's path (`monitor/<resource>`): its attributes are the app's fields
 * (read-only ones `readonly`) and its methods are the app's actions, named in camel
 * case and taking the arguments the wire takes them under. An action that takes
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "@tilo/sdk";
import type * as tilo from "@tilo/sdk";

/** One of "pid", "name", "user", "cpu", "mem", "threads". */
export type SortKey = "pid" | "name" | "user" | "cpu" | "mem" | "threads";

/** Processes — `monitor.processes`. */
export interface Processes extends tilo.Resource {
  sort: SortKey;
  ascending: boolean;
  flat: boolean;
  // the app's `refresh` action is the SDK's own: run it with `action("refresh")`.
  showTab(args: {
    tab: string;
  }): Promise<void>;
  filter(args?: {
    query?: string;
  }): Promise<void>;
  inspect(args: {
    process: string;
  }): Promise<void>;
  copyPid(args: {
    process: string;
  }): Promise<void>;
}

/** The `monitor.processes` kind at `path`. */
export function processes(ws: tilo.Workspace, path: string): Processes {
  return resourceProxy(ws, path) as unknown as Processes;
}

