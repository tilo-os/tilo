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
export const SURFACE = "so.tilo.totp@78ecd43e7f72fa75";

/** One of "Sha1", "Sha256", "Sha512". */
export type Algorithm = "Sha1" | "Sha256" | "Sha512";

export interface StoredAccount {
  issuer: string;
  label: string;
  algorithm: Algorithm;
  digits: number;
  period: number;
  color: number;
}

/** The arguments of `move-up`. */
export interface MoveUpArgs {
  account: string;
}

/** The arguments of `move-down`. */
export interface MoveDownArgs {
  account: string;
}

/** The arguments of `delete`. */
export interface DeleteArgs {
  account: string;
}

/** Authenticator — `totp.vault`. */
export interface Vault extends tilo.Resource {
  accounts: Array<tilo.Entry<StoredAccount>>;
  moveUp(args: {
    account: string;
  }): Promise<void>;
  /** Runs `move-up` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  moveUpMany(items: MoveUpArgs[]): Promise<tilo.ActionAnswer[]>;
  moveDown(args: {
    account: string;
  }): Promise<void>;
  /** Runs `move-down` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  moveDownMany(items: MoveDownArgs[]): Promise<tilo.ActionAnswer[]>;
  delete(args: {
    account: string;
  }): Promise<void>;
  /** Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteMany(items: DeleteArgs[]): Promise<tilo.ActionAnswer[]>;
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

/** The actions `Vault` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const VAULT_ACTIONS = ["moveUp", "moveUpMany", "moveDown", "moveDownMany", "delete", "deleteMany", "addAccount", "openAccount", "copyCode", "setUpLocal"] as const;

/** The `totp.vault` kind at `path`. */
export function vault(ws: tilo.Workspace, path: string): Vault {
  return resourceProxy(ws, path, SURFACE, VAULT_ACTIONS) as unknown as Vault;
}

