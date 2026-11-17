/**
 * Types for Disks (`so.tilo.disks`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `System` is a `Resource` of the app's `disks.system`, made by its factory with the
 * resource's path (`disks/<resource>`): its attributes are the app's fields
 * (read-only ones `readonly`) and its methods are the app's actions, named in camel
 * case and taking the arguments the wire takes them under. An action that takes
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is. An undoable action that answers nothing also has a `<action>Many`,
which applies it to every argument object of a list in a few all-or-nothing steps,
all of them one run.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** The version of the surface these types were made from: a workspace whose app
 * publishes another refuses their actions rather than run what they no longer
 * describe. */
export const SURFACE = "so.tilo.disks@d7ae32858694d31a";

/** The arguments of `name-drive`. */
export interface NameDriveArgs {
  serial: string;
  name?: string;
}

/** Storage devices — `disks.system`. */
export interface System extends tilo.Resource {
  names: Record<string, string>;
  nameDrive(args: {
    serial: string;
    name?: string;
  }): Promise<void>;
  /** Runs `name-drive` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  nameDriveMany(items: NameDriveArgs[]): Promise<tilo.ActionAnswer[]>;
  rescan(): Promise<void>;
  select(args: {
    device: string;
  }): Promise<void>;
  showTab(args: {
    tab: string;
  }): Promise<void>;
}

/** The actions `System` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SYSTEM_ACTIONS = ["nameDrive", "nameDriveMany", "rescan", "select", "showTab"] as const;

/** The `disks.system` kind at `path`. */
export function system(ws: tilo.Workspace, path: string): System {
  return resourceProxy(ws, path, SURFACE, SYSTEM_ACTIONS) as unknown as System;
}

