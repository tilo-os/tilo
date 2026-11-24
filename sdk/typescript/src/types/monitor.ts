/**
 * Types for System Monitor (`so.tilo.monitor`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Row` is a `Resource` of the app's `tl.so.tilo.monitor.Row`, made by its factory with the
 * resource's path (`monitor/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.monitor@9c68f8bfdaae9287";

/** Row — `tl.so.tilo.monitor.Row`. */
export interface Row extends tilo.Resource {
  uoc4p4dx: unknown;  // process
  qwzsalow: number;  // depth
  "6waph57m": boolean;  // has_children
}

/** The actions `Row` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ROW_ACTIONS = [] as const;

/** The `tl.so.tilo.monitor.Row` kind at `path`. */
export function row(ws: tilo.Workspace, path: string): Row {
  return resourceProxy(ws, path, SURFACE, ROW_ACTIONS) as unknown as Row;
}

/** Monitor — `tl.so.tilo.monitor.Monitor`. */
export interface Monitor extends tilo.Resource {
}

/** The actions `Monitor` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MONITOR_ACTIONS = [] as const;

/** The `tl.so.tilo.monitor.Monitor` kind at `path`. */
export function monitor(ws: tilo.Workspace, path: string): Monitor {
  return resourceProxy(ws, path, SURFACE, MONITOR_ACTIONS) as unknown as Monitor;
}

/** System Monitor — `tl.so.tilo.monitor`. */
export interface TiloMonitor extends tilo.Resource {
  /** Asks every process of an app to close. */
  end_app(args: {
    application: string;
  }): Promise<void>;
}

/** The actions `TiloMonitor` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TILOMONITOR_ACTIONS = ["end_app"] as const;

/** The `tl.so.tilo.monitor` kind at `path`. */
export function tiloMonitor(ws: tilo.Workspace, path: string): TiloMonitor {
  return resourceProxy(ws, path, SURFACE, TILOMONITOR_ACTIONS) as unknown as TiloMonitor;
}

