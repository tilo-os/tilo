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

/** The version of the surface these types were made from: a workspace whose app
 * publishes another refuses their actions rather than run what they no longer
 * describe. */
export const SURFACE = "so.tilo.publisher@a369a701e1193f6d";

/** App Publisher — `publisher.profile`. */
export interface Profile extends tilo.Resource {
  registry: string;
  domain: string;
  forge_account: string;
  contact: string;
}

/** The `publisher.profile` kind at `path`. */
export function profile(ws: tilo.Workspace, path: string): Profile {
  return resourceProxy(ws, path, SURFACE) as unknown as Profile;
}

