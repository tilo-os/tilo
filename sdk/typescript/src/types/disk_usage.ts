/**
 * Types for Disk Usage Analyzer (`so.tilo.disk-usage`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `DiskUsage` is a `Resource` of the app's `disk-usage`, made by its factory with the
 * resource's path (`disk-usage/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.disk-usage@710d1950408d78af";

/** The arguments of `remember`. */
export interface RememberArgs {
  device: string;
  folder: string;
}

/** Disk Usage Analyzer — `disk-usage`. */
export interface DiskUsage extends tilo.Resource {
}

/** The actions `DiskUsage` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DISKUSAGE_ACTIONS = [] as const;

/** The `disk-usage` kind at `path`. */
export function diskUsage(ws: tilo.Workspace, path: string): DiskUsage {
  return resourceProxy(ws, path, SURFACE, DISKUSAGE_ACTIONS) as unknown as DiskUsage;
}

/** Recent folders — `disk-usage.recent-folders`. */
export interface RecentFolders extends tilo.Resource {
  devices: Record<string, unknown>;
  remember(args: {
    device: string;
    folder: string;
  }): Promise<void>;
  /** Runs `remember` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rememberMany(items: RememberArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `RecentFolders` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RECENTFOLDERS_ACTIONS = ["remember", "rememberMany"] as const;

/** The `disk-usage.recent-folders` kind at `path`. */
export function recentFolders(ws: tilo.Workspace, path: string): RecentFolders {
  return resourceProxy(ws, path, SURFACE, RECENTFOLDERS_ACTIONS) as unknown as RecentFolders;
}

