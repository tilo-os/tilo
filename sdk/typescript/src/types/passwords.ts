/**
 * Types for Passwords (`so.tilo.passwords`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Vault` is a `Resource` of the app's `passwords.vault`, made by its factory with the
 * resource's path (`passwords/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.passwords@f1fa1743f50c3e74";

/** Password vault — `passwords.vault`. */
export interface Vault extends tilo.Resource {
  denyAccess(args: {
    proposal: string;
  }): Promise<void>;
  add(): Promise<void>;
  listLogins(): Promise<void>;
  cancel(): Promise<void>;
  copy(args: {
    item: string;
  }): Promise<void>;
  copyIdentity(args: {
    item: string;
  }): Promise<void>;
  copyOrigins(args: {
    item: string;
  }): Promise<void>;
  reveal(args: {
    item: string;
  }): Promise<void>;
  filter(args: {
    filter: string;
  }): Promise<void>;
}

/** The `passwords.vault` kind at `path`. */
export function vault(ws: tilo.Workspace, path: string): Vault {
  return resourceProxy(ws, path, SURFACE) as unknown as Vault;
}

