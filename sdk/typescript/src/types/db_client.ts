/**
 * Types for Database Client (`so.tilo.db-client`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Workspace` is a `Resource` of the app's `db-client.workspace`, made by its factory with the
 * resource's path (`db-client/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.db-client@f4a9771cec06f9d0";

/** One of "sqlite", "postgres", "mysql". */
export type Engine = "sqlite" | "postgres" | "mysql";

export interface SavedConnection {
  name: string;
  engine: Engine;
  host: string;
  port: number;
  database: string;
  user: string;
  file_path: string;
}

export interface SavedQuery {
  name: string;
  sql: string;
}

/** The arguments of `add-connection`. */
export interface AddConnectionArgs {
  name: string;
  engine: string;
  location?: string;
  database?: string;
  user?: string;
}

/** The arguments of `remove-connection`. */
export interface RemoveConnectionArgs {
  connection: string;
}

/** The arguments of `save-query`. */
export interface SaveQueryArgs {
  name: string;
  sql?: string;
}

/** The arguments of `remove-query`. */
export interface RemoveQueryArgs {
  query: string;
}

/** Database — `db-client.workspace`. */
export interface Workspace extends tilo.Resource {
  connections: Array<tilo.Entry<SavedConnection>>;
  saved_queries: Array<tilo.Entry<SavedQuery>>;
  sql: string;
  addConnection(args: {
    name: string;
    engine: string;
    location?: string;
    database?: string;
    user?: string;
  }): Promise<void>;
  /** Runs `add-connection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  addConnectionMany(items: AddConnectionArgs[]): Promise<tilo.ActionAnswer[]>;
  removeConnection(args: {
    connection: string;
  }): Promise<void>;
  /** Runs `remove-connection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  removeConnectionMany(items: RemoveConnectionArgs[]): Promise<tilo.ActionAnswer[]>;
  saveQuery(args: {
    name: string;
    sql?: string;
  }): Promise<void>;
  /** Runs `save-query` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  saveQueryMany(items: SaveQueryArgs[]): Promise<tilo.ActionAnswer[]>;
  removeQuery(args: {
    query: string;
  }): Promise<void>;
  /** Runs `remove-query` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  removeQueryMany(items: RemoveQueryArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Workspace` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WORKSPACE_ACTIONS = ["addConnection", "addConnectionMany", "removeConnection", "removeConnectionMany", "saveQuery", "saveQueryMany", "removeQuery", "removeQueryMany"] as const;

/** The `db-client.workspace` kind at `path`. */
export function workspace(ws: tilo.Workspace, path: string): Workspace {
  return resourceProxy(ws, path, SURFACE, WORKSPACE_ACTIONS) as unknown as Workspace;
}

