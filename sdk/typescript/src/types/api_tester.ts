/**
 * Types for API Tester (`so.tilo.api-tester`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Workspace` is a `Resource` of the app's `api-tester.workspace`, made by its factory with the
 * resource's path (`api-tester/<resource>`): its attributes are the app's fields
 * (read-only ones `readonly`) and its methods are the app's actions, named in camel
 * case and taking the arguments the wire takes them under. An action that takes
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** The version of the surface these types were made from: a workspace whose app
 * publishes another refuses their actions rather than run what they no longer
 * describe. */
export const SURFACE = "so.tilo.api-tester@2dc7adfdb370ac66";

/** One of "none", "bearer", "basic", "api_key". */
export type AuthScheme = "none" | "bearer" | "basic" | "api_key";

/** One of "none", "json", "form", "raw". */
export type BodyMode = "none" | "json" | "form" | "raw";

export interface Credentials {
  scheme: AuthScheme;
  username: string;
  header: string;
  reference: string;
}

export interface Environment {
  name: string;
  variables: Variable[];
}

/** One of "get", "post", "put", "patch", "delete", "head", "options". */
export type HttpMethod = "get" | "post" | "put" | "patch" | "delete" | "head" | "options";

export interface RequestRecord {
  name: string;
  method: HttpMethod;
  url: string;
  params: Row[];
  headers: Row[];
  auth: Credentials;
  body_mode: BodyMode;
  body: string;
  form: Row[];
}

export interface Row {
  key: string;
  value: string;
  enabled: boolean;
}

export interface SavedCollection {
  name: string;
  requests: RequestRecord[];
}

export interface Sent {
  route: string;
  method: HttpMethod;
  url: string;
  status: number;
  duration_ms: number;
  at_ms: number;
  replay: string;
}

export interface Variable {
  key: string;
  value: string;
  enabled: boolean;
  secret: boolean;
}

/** API workspace — `api-tester.workspace`. */
export interface Workspace extends tilo.Resource {
  collections: SavedCollection[];
  environments: Environment[];
  active_environment: string | null;
  request: RequestRecord;
  readonly opened: string;
  readonly history: Sent[];
  importWorkspace(args: {
    workspace: unknown;
  }): Promise<tilo.Run | void>;
  newRequest(): Promise<tilo.Run | void>;
  newCollection(args: {
    name: string;
  }): Promise<tilo.Run | void>;
  deleteCollection(args: {
    collection: string;
  }): Promise<tilo.Run | void>;
  addRequest(args: {
    collection: string;
    name: string;
  }): Promise<tilo.Run | void>;
  saveRequest(args: {
    collection: string;
    name?: string;
  }): Promise<tilo.Run | void>;
  openRequest(args: {
    collection: string;
    request: string;
  }): Promise<tilo.Run | void>;
  deleteRequest(args: {
    collection: string;
    request: string;
  }): Promise<tilo.Run | void>;
  newEnvironment(args: {
    name: string;
  }): Promise<tilo.Run | void>;
  deleteEnvironment(args: {
    environment: string;
  }): Promise<tilo.Run | void>;
  useEnvironment(args: {
    environment: string;
  }): Promise<tilo.Run | void>;
  addVariable(args: {
    environment: string;
    key: string;
    value?: string;
    secret?: boolean;
  }): Promise<tilo.Run | void>;
  openSent(args: {
    sent: string;
  }): Promise<tilo.Run | void>;
  saveSent(args: {
    collection: string;
    sent: string;
    name?: string;
  }): Promise<tilo.Run | void>;
  deleteSent(args: {
    sent: string;
  }): Promise<tilo.Run | void>;
  recordSent(args: {
    method: string;
    url: string;
    status: number;
    duration_ms: number;
  }): Promise<tilo.Run | void>;
}

/** The `api-tester.workspace` kind at `path`. */
export function workspace(ws: tilo.Workspace, path: string): Workspace {
  return resourceProxy(ws, path, SURFACE) as unknown as Workspace;
}

