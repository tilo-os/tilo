/**
 * Types for Remote Desktop (`so.tilo.remote`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `ConnectionList` is a `Resource` of the app's `remote.connection-list`, made by its factory with the
 * resource's path (`remote/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.remote@ab22d8658af621c3";

/** One of "password", "public_key", "device_identity". */
export type AuthMethod = "password" | "public_key" | "device_identity";

export interface Connection {
  name: string;
  host: string;
  port: number;
  user: string;
  kind: ConnectionKind;
  auth: AuthMethod;
  last_used_ms: number | null;
  group: string;
  favorite: boolean;
}

/** One of "ssh", "vnc", "rdp", "tilo". */
export type ConnectionKind = "ssh" | "vnc" | "rdp" | "tilo";

/** The arguments of `add-connection`. */
export interface AddConnectionArgs {
  name: string;
  host: string;
  kind: ConnectionKind;
  port?: number;
  user?: string;
}

/** The arguments of `delete-connection`. */
export interface DeleteConnectionArgs {
  connection: string;
}

/** The arguments of `mark-used`. */
export interface MarkUsedArgs {
  connection: string;
  at_ms: number;
}

/** Remote connections — `remote.connection-list`. */
export interface ConnectionList extends tilo.Resource {
  connections: Array<tilo.Entry<Connection>>;
  addConnection(args: {
    name: string;
    host: string;
    kind: ConnectionKind;
    port?: number;
    user?: string;
  }): Promise<void>;
  /** Runs `add-connection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  addConnectionMany(items: AddConnectionArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteConnection(args: {
    connection: string;
  }): Promise<void>;
  /** Runs `delete-connection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteConnectionMany(items: DeleteConnectionArgs[]): Promise<tilo.ActionAnswer[]>;
  markUsed(args: {
    connection: string;
    at_ms: number;
  }): Promise<void>;
  /** Runs `mark-used` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  markUsedMany(items: MarkUsedArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `ConnectionList` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CONNECTIONLIST_ACTIONS = ["addConnection", "addConnectionMany", "deleteConnection", "deleteConnectionMany", "markUsed", "markUsedMany"] as const;

/** The `remote.connection-list` kind at `path`. */
export function connectionList(ws: tilo.Workspace, path: string): ConnectionList {
  return resourceProxy(ws, path, SURFACE, CONNECTIONLIST_ACTIONS) as unknown as ConnectionList;
}

