/**
 * Types for Windows Apps (`so.tilo.windows`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`windows/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.windows@03167a1d76fc0991";

/** How a program opens: with Wine or Proton in a private capsule, or in the Windows appliance. One of "wine", "proton", "vm". */
export type OpenWith = "wine" | "proton" | "vm";

/** Notice — `tl.tilo.host.Notice`. */
export interface Notice extends tilo.Resource {
  notice_to: string;  // to
  notice_title: string;  // title
  notice_body: string;  // body
  notice_opens: unknown;  // opens
  notice_seen: boolean;  // seen
  notice_device: string;  // device
  notice_actions: string[];  // actions
}

/** The actions `Notice` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const NOTICE_ACTIONS = [] as const;

/** The `tl.tilo.host.Notice` kind at `path`. */
export function notice(ws: tilo.Workspace, path: string): Notice {
  return resourceProxy(ws, path, SURFACE, NOTICE_ACTIONS) as unknown as Notice;
}

/** Windows — `tl.so.tilo.windows.Windows`. */
export interface Windows extends tilo.Resource {
  "3bu5zzlt": Record<string, OpenWith>;  // routes
  /** Choose how a program opens from now on, and carry the choice to this computer. */
  set_route(args: {
    program: string;
    route: OpenWith;
  }): Promise<void>;
  /** Let a program open the way the catalogue recommends, and carry that to this computer. */
  use_recommendation(args: {
    program: string;
  }): Promise<void>;
  /** Install the installer the person chose, or the one a file manager opened here. */
  installed(args: {
    file: unknown;
  }): Promise<void>;
  /** Open the document the person chose in `program`. */
  opened_file(args: {
    program: unknown;
    file: unknown;
  }): Promise<void>;
}

/** The actions `Windows` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WINDOWS_ACTIONS = ["set_route", "use_recommendation", "installed", "opened_file"] as const;

/** The `tl.so.tilo.windows.Windows` kind at `path`. */
export function windows(ws: tilo.Workspace, path: string): Windows {
  return resourceProxy(ws, path, SURFACE, WINDOWS_ACTIONS) as unknown as Windows;
}

/** Windows Apps — `tl.so.tilo.windows`. */
export interface TiloWindows extends tilo.Resource {
  /** Set up the appliance from the Windows ISO the person chose. */
  imported_iso(args: {
    file: unknown;
  }): Promise<void>;
  /** Open the appliance's recovery desktop. */
  recovery_desktop(): Promise<void>;
  /** Start the appliance. */
  start_windows(): Promise<void>;
  /** Restart the appliance. */
  restart_windows(): Promise<void>;
  /** Suspend the appliance. */
  suspend_windows(): Promise<void>;
  /** Resume the appliance. */
  resume_windows(): Promise<void>;
  /** Shut the appliance down. */
  shutdown_windows(): Promise<void>;
  /** Connect the appliance for activation, for a while. */
  temporary_network(): Promise<void>;
  /** Connect the appliance for Windows Update, for a while. */
  update_network(): Promise<void>;
  /** End the appliance's temporary connection. */
  revoke_network(): Promise<void>;
}

/** The actions `TiloWindows` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TILOWINDOWS_ACTIONS = ["imported_iso", "recovery_desktop", "start_windows", "restart_windows", "suspend_windows", "resume_windows", "shutdown_windows", "temporary_network", "update_network", "revoke_network"] as const;

/** The `tl.so.tilo.windows` kind at `path`. */
export function tiloWindows(ws: tilo.Workspace, path: string): TiloWindows {
  return resourceProxy(ws, path, SURFACE, TILOWINDOWS_ACTIONS) as unknown as TiloWindows;
}

