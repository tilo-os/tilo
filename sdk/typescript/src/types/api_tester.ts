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
 * says where it is. An undoable action that answers nothing also has a `<action>Many`,
which applies it to every argument object of a list in a few all-or-nothing steps,
all of them one run.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** The version of the surface these types were made from: a workspace whose app
 * publishes another refuses their actions rather than run what they no longer
 * describe. */
export const SURFACE = "so.tilo.api-tester@d6b32f527d6a5c3a";

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

/** The arguments of `new-request`. */
export interface NewRequestArgs {
}

/** The arguments of `new-collection`. */
export interface NewCollectionArgs {
  name: string;
}

/** The arguments of `delete-collection`. */
export interface DeleteCollectionArgs {
  collection: string;
}

/** The arguments of `add-request`. */
export interface AddRequestArgs {
  collection: string;
  name: string;
}

/** The arguments of `save-request`. */
export interface SaveRequestArgs {
  collection: string;
  name?: string;
}

/** The arguments of `open-request`. */
export interface OpenRequestArgs {
  collection: string;
  request: string;
}

/** The arguments of `delete-request`. */
export interface DeleteRequestArgs {
  collection: string;
  request: string;
}

/** The arguments of `new-environment`. */
export interface NewEnvironmentArgs {
  name: string;
}

/** The arguments of `delete-environment`. */
export interface DeleteEnvironmentArgs {
  environment: string;
}

/** The arguments of `use-environment`. */
export interface UseEnvironmentArgs {
  environment: string;
}

/** The arguments of `add-variable`. */
export interface AddVariableArgs {
  environment: string;
  key: string;
  value?: string;
  secret?: boolean;
}

/** The arguments of `open-sent`. */
export interface OpenSentArgs {
  sent: string;
}

/** The arguments of `save-sent`. */
export interface SaveSentArgs {
  collection: string;
  sent: string;
  name?: string;
}

/** The arguments of `delete-sent`. */
export interface DeleteSentArgs {
  sent: string;
}

/** The arguments of `record-sent`. */
export interface RecordSentArgs {
  method: string;
  url: string;
  status: number;
  duration_ms: number;
}

/** API workspace — `api-tester.workspace`. */
export interface Workspace extends tilo.Resource {
  collections: SavedCollection[];
  environments: Environment[];
  active_environment: string | null;
  request: RequestRecord;
  readonly opened: string;
  readonly history: Sent[];
  newRequest(): Promise<tilo.Run | void>;
  /** Runs `new-request` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  newRequestMany(items: NewRequestArgs[]): Promise<tilo.ActionAnswer[]>;
  newCollection(args: {
    name: string;
  }): Promise<tilo.Run | void>;
  /** Runs `new-collection` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  newCollectionMany(items: NewCollectionArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteCollection(args: {
    collection: string;
  }): Promise<tilo.Run | void>;
  /** Runs `delete-collection` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  deleteCollectionMany(items: DeleteCollectionArgs[]): Promise<tilo.ActionAnswer[]>;
  addRequest(args: {
    collection: string;
    name: string;
  }): Promise<tilo.Run | void>;
  /** Runs `add-request` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  addRequestMany(items: AddRequestArgs[]): Promise<tilo.ActionAnswer[]>;
  saveRequest(args: {
    collection: string;
    name?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `save-request` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  saveRequestMany(items: SaveRequestArgs[]): Promise<tilo.ActionAnswer[]>;
  openRequest(args: {
    collection: string;
    request: string;
  }): Promise<tilo.Run | void>;
  /** Runs `open-request` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  openRequestMany(items: OpenRequestArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteRequest(args: {
    collection: string;
    request: string;
  }): Promise<tilo.Run | void>;
  /** Runs `delete-request` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  deleteRequestMany(items: DeleteRequestArgs[]): Promise<tilo.ActionAnswer[]>;
  newEnvironment(args: {
    name: string;
  }): Promise<tilo.Run | void>;
  /** Runs `new-environment` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  newEnvironmentMany(items: NewEnvironmentArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteEnvironment(args: {
    environment: string;
  }): Promise<tilo.Run | void>;
  /** Runs `delete-environment` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  deleteEnvironmentMany(items: DeleteEnvironmentArgs[]): Promise<tilo.ActionAnswer[]>;
  useEnvironment(args: {
    environment: string;
  }): Promise<tilo.Run | void>;
  /** Runs `use-environment` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  useEnvironmentMany(items: UseEnvironmentArgs[]): Promise<tilo.ActionAnswer[]>;
  addVariable(args: {
    environment: string;
    key: string;
    value?: string;
    secret?: boolean;
  }): Promise<tilo.Run | void>;
  /** Runs `add-variable` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  addVariableMany(items: AddVariableArgs[]): Promise<tilo.ActionAnswer[]>;
  openSent(args: {
    sent: string;
  }): Promise<tilo.Run | void>;
  /** Runs `open-sent` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  openSentMany(items: OpenSentArgs[]): Promise<tilo.ActionAnswer[]>;
  saveSent(args: {
    collection: string;
    sent: string;
    name?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `save-sent` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  saveSentMany(items: SaveSentArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteSent(args: {
    sent: string;
  }): Promise<tilo.Run | void>;
  /** Runs `delete-sent` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  deleteSentMany(items: DeleteSentArgs[]): Promise<tilo.ActionAnswer[]>;
  recordSent(args: {
    method: string;
    url: string;
    status: number;
    duration_ms: number;
  }): Promise<tilo.Run | void>;
  /** Runs `record-sent` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  recordSentMany(items: RecordSentArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The `api-tester.workspace` kind at `path`. */
export function workspace(ws: tilo.Workspace, path: string): Workspace {
  return resourceProxy(ws, path, SURFACE) as unknown as Workspace;
}

