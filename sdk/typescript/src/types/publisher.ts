/**
 * Types for App Publisher (`so.tilo.publisher`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Profile` is a `Resource` of the app's `publisher.profile`, made by its factory with the
 * resource's path (`publisher/<resource>`): its attributes are the app's fields
 * (read-only ones `readonly`) and its methods are the app's actions, named in camel
 * case and taking the arguments the wire takes them under. An action that takes
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** App Publisher — `publisher.profile`. */
export interface Profile extends tilo.Resource {
  registry: unknown;
  domain: unknown;
  forge_account: unknown;
  contact: unknown;
}

/** The `publisher.profile` kind at `path`. */
export function profile(ws: tilo.Workspace, path: string): Profile {
  return resourceProxy(ws, path) as unknown as Profile;
}

