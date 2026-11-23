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
export const SURFACE = "so.tilo.passwords@8ac1e6ef206532c5";

/** The arguments of `keep-sealed`. */
export interface KeepSealedArgs {
  id: string;
  ciphertext: string;
}

/** The arguments of `import-sealed`. */
export interface ImportSealedArgs {
  id: string;
  ciphertext: string;
}

/** The arguments of `remove-sealed`. */
export interface RemoveSealedArgs {
  id: string;
}

/** The arguments of `keep-uses`. */
export interface KeepUsesArgs {
  id: string;
  ciphertext: string;
}

/** Password vault — `passwords.vault`. */
export interface Vault extends tilo.Resource {
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

/** The actions `Vault` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const VAULT_ACTIONS = ["add", "listLogins", "cancel", "copy", "copyIdentity", "copyOrigins", "reveal", "filter"] as const;

/** The `passwords.vault` kind at `path`. */
export function vault(ws: tilo.Workspace, path: string): Vault {
  return resourceProxy(ws, path, SURFACE, VAULT_ACTIONS) as unknown as Vault;
}

/** Workspace vault — `passwords.workspace-vault`. */
export interface WorkspaceVault extends tilo.Resource {
  readonly sealed: Array<tilo.Entry<string>>;
  readonly imported: Array<tilo.Entry<string>>;
  readonly removed: string[];
  readonly used: Array<tilo.Entry<string>>;
  keepSealed(args: {
    id: string;
    ciphertext: string;
  }): Promise<void>;
  /** Runs `keep-sealed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  keepSealedMany(items: KeepSealedArgs[]): Promise<tilo.ActionAnswer[]>;
  importSealed(args: {
    id: string;
    ciphertext: string;
  }): Promise<void>;
  /** Runs `import-sealed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  importSealedMany(items: ImportSealedArgs[]): Promise<tilo.ActionAnswer[]>;
  removeSealed(args: {
    id: string;
  }): Promise<void>;
  /** Runs `remove-sealed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  removeSealedMany(items: RemoveSealedArgs[]): Promise<tilo.ActionAnswer[]>;
  keepUses(args: {
    id: string;
    ciphertext: string;
  }): Promise<void>;
  /** Runs `keep-uses` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  keepUsesMany(items: KeepUsesArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `WorkspaceVault` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WORKSPACEVAULT_ACTIONS = ["keepSealed", "keepSealedMany", "importSealed", "importSealedMany", "removeSealed", "removeSealedMany", "keepUses", "keepUsesMany"] as const;

/** The `passwords.workspace-vault` kind at `path`. */
export function workspaceVault(ws: tilo.Workspace, path: string): WorkspaceVault {
  return resourceProxy(ws, path, SURFACE, WORKSPACEVAULT_ACTIONS) as unknown as WorkspaceVault;
}

