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
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** Remote connections — `remote.connection-list`. */
export interface ConnectionList extends tilo.Resource {
  connections: unknown;
  addConnection(args: {
    name: string;
    host: string;
    kind: unknown;
    port?: number;
    user?: string;
  }): Promise<tilo.Run | void>;
  deleteConnection(args: {
    connection: string;
  }): Promise<tilo.Run | void>;
  markUsed(args: {
    connection: string;
    at_ms: number;
  }): Promise<tilo.Run | void>;
}

/** The `remote.connection-list` kind at `path`. */
export function connectionList(ws: tilo.Workspace, path: string): ConnectionList {
  return resourceProxy(ws, path) as unknown as ConnectionList;
}

