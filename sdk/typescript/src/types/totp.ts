/**
 * Types for Authenticator (`so.tilo.totp`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Vault` is a `Resource` of the app's `totp.vault`, made by its factory with the
 * resource's path (`totp/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.totp@7557a43b95820399";

/** One of "sha1", "sha256", "sha512". */
export type Algorithm = "sha1" | "sha256" | "sha512";

export interface StoredAccount {
  issuer: string;
  label: string;
  algorithm: Algorithm;
  digits: number;
  period: number;
  color: number;
}

/** Authenticator — `totp.vault`. */
export interface Vault extends tilo.Resource {
  accounts: StoredAccount[];
  moveUp(args: {
    account: string;
  }): Promise<tilo.Run | void>;
  moveDown(args: {
    account: string;
  }): Promise<tilo.Run | void>;
  delete(args: {
    account: string;
  }): Promise<tilo.Run | void>;
  addAccount(): Promise<void>;
  openAccount(args: {
    account: string;
  }): Promise<void>;
  copyCode(args: {
    account: string;
  }): Promise<void>;
  setUpLocal(args: {
    account: string;
  }): Promise<void>;
}

/** The `totp.vault` kind at `path`. */
export function vault(ws: tilo.Workspace, path: string): Vault {
  return resourceProxy(ws, path, SURFACE) as unknown as Vault;
}

