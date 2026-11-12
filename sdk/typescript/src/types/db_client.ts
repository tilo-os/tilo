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
 * says where it is.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** Database — `db-client.workspace`. */
export interface Workspace extends tilo.Resource {
  connections: unknown;
  saved_queries: unknown;
  sql: unknown;
  addConnection(args: {
    name: string;
    engine: string;
    location?: string;
    database?: string;
    user?: string;
  }): Promise<tilo.Run | void>;
  removeConnection(args: {
    connection: string;
  }): Promise<tilo.Run | void>;
  saveQuery(args: {
    name: string;
    sql?: string;
  }): Promise<tilo.Run | void>;
  removeQuery(args: {
    query: string;
  }): Promise<tilo.Run | void>;
}

/** The `db-client.workspace` kind at `path`. */
export function workspace(ws: tilo.Workspace, path: string): Workspace {
  return resourceProxy(ws, path) as unknown as Workspace;
}

