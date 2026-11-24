/**
 * Types for Disks (`so.tilo.disks`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`disks/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.disks@5d78ccc3c7eb6c6b";

/** The arguments of `name_drive`. */
export interface NameDriveArgs {
  serial: string;
  name: string;
}

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

/** Disks — `tl.so.tilo.disks.Disks`. */
export interface Disks extends tilo.Resource {
  "6mf2skfm": Record<string, string>;  // names
  /** A drive's name in the words of the person reading it, by its serial. */
  name_drive(args: {
    serial: string;
    name: string;
  }): Promise<void>;
  /** Runs `name_drive` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  name_driveMany(items: NameDriveArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens a mounted folder in Files. */
  open_in_files(args: {
    path: string;
  }): Promise<void>;
  /** Opens Disk Usage, where a folder's use is measured. */
  open_usage(): Promise<void>;
}

/** The actions `Disks` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DISKS_ACTIONS = ["name_drive", "name_driveMany", "open_in_files", "open_usage"] as const;

/** The `tl.so.tilo.disks.Disks` kind at `path`. */
export function disks(ws: tilo.Workspace, path: string): Disks {
  return resourceProxy(ws, path, SURFACE, DISKS_ACTIONS) as unknown as Disks;
}

/** Disks — `tl.so.tilo.disks`. */
export interface TiloDisks extends tilo.Resource {
  /** Mounts the drive's first partition, which is what a drive without a filesystem of its own holds. */
  mount_first(args: {
    drive: unknown;
  }): Promise<void>;
  /** Erases the device the confirmation named, and writes the chosen filesystem. */
  format_device(args: {
    device_name: string;
    filesystem: string;
  }): Promise<void>;
}

/** The actions `TiloDisks` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TILODISKS_ACTIONS = ["mount_first", "format_device"] as const;

/** The `tl.so.tilo.disks` kind at `path`. */
export function tiloDisks(ws: tilo.Workspace, path: string): TiloDisks {
  return resourceProxy(ws, path, SURFACE, TILODISKS_ACTIONS) as unknown as TiloDisks;
}

