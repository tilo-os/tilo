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
export const SURFACE = "so.tilo.db-client@ff2837e66ce4dd02";

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
  connections: SavedConnection[];
  saved_queries: SavedQuery[];
  sql: string;
  addConnection(args: {
    name: string;
    engine: string;
    location?: string;
    database?: string;
    user?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `add-connection` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  addConnectionMany(items: AddConnectionArgs[]): Promise<tilo.ActionAnswer[]>;
  removeConnection(args: {
    connection: string;
  }): Promise<tilo.Run | void>;
  /** Runs `remove-connection` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  removeConnectionMany(items: RemoveConnectionArgs[]): Promise<tilo.ActionAnswer[]>;
  saveQuery(args: {
    name: string;
    sql?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `save-query` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  saveQueryMany(items: SaveQueryArgs[]): Promise<tilo.ActionAnswer[]>;
  removeQuery(args: {
    query: string;
  }): Promise<tilo.Run | void>;
  /** Runs `remove-query` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  removeQueryMany(items: RemoveQueryArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The `db-client.workspace` kind at `path`. */
export function workspace(ws: tilo.Workspace, path: string): Workspace {
  return resourceProxy(ws, path, SURFACE) as unknown as Workspace;
}

