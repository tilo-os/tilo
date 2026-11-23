/**
 * Types for Settings (`so.tilo.settings`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Root` is a `Resource` of the app's `settings.root`, made by its factory with the
 * resource's path (`settings/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.settings@2edd3868103ec032";

export interface ListedAccount {
  type_id: string;
  display_name: string;
  config: unknown;
  enabled: boolean;
  tint: number;
}

/** The arguments of `publish-browser-identity`. */
export interface PublishBrowserIdentityArgs {
  root: string;
  origin: string;
  display_name: string;
}

/** The arguments of `save-account`. */
export interface SaveAccountArgs {
  id: string;
  type_id: string;
  display_name: string;
  config: unknown;
  enabled: boolean;
  tint: number;
}

/** The arguments of `remove-account`. */
export interface RemoveAccountArgs {
  account: string;
}

/** Settings — `settings.root`. */
export interface Root extends tilo.Resource {
  inspectInputSources(args?: {
    query?: string;
    page?: number;
  }): Promise<void>;
  inspectKeyboardMap(): Promise<void>;
  inspectKeyboardSources(): Promise<void>;
  setSourceOptionsVisible(args: {
    visible: boolean;
  }): Promise<void>;
  setSourceInspectionVisible(args: {
    visible: boolean;
  }): Promise<void>;
  openSourcePicker(): Promise<void>;
  showLiveKeyboardMap(): Promise<void>;
  storageSummary(): Promise<void>;
  searchHubModels(args: {
    query: string;
    format?: string;
  }): Promise<void>;
  showHubModel(args: {
    model: string;
  }): Promise<void>;
  toggleMicrophoneTest(): Promise<void>;
  inspectCalendarChoices(): Promise<void>;
  inspectDisplayConfiguration(): Promise<void>;
}

/** The actions `Root` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ROOT_ACTIONS = ["inspectInputSources", "inspectKeyboardMap", "inspectKeyboardSources", "setSourceOptionsVisible", "setSourceInspectionVisible", "openSourcePicker", "showLiveKeyboardMap", "storageSummary", "searchHubModels", "showHubModel", "toggleMicrophoneTest", "inspectCalendarChoices", "inspectDisplayConfiguration"] as const;

/** The `settings.root` kind at `path`. */
export function root(ws: tilo.Workspace, path: string): Root {
  return resourceProxy(ws, path, SURFACE, ROOT_ACTIONS) as unknown as Root;
}

/** Accounts — `tilo.accounts`. */
export interface Accounts extends tilo.Resource {
  accounts: Array<tilo.Entry<ListedAccount>>;
  publishBrowserIdentity(args: {
    root: string;
    origin: string;
    display_name: string;
  }): Promise<void>;
  /** Runs `publish-browser-identity` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  publishBrowserIdentityMany(items: PublishBrowserIdentityArgs[]): Promise<tilo.ActionAnswer[]>;
  saveAccount(args: {
    id: string;
    type_id: string;
    display_name: string;
    config: unknown;
    enabled: boolean;
    tint: number;
  }): Promise<void>;
  /** Runs `save-account` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  saveAccountMany(items: SaveAccountArgs[]): Promise<tilo.ActionAnswer[]>;
  removeAccount(args: {
    account: string;
  }): Promise<void>;
  /** Runs `remove-account` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  removeAccountMany(items: RemoveAccountArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Accounts` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ACCOUNTS_ACTIONS = ["publishBrowserIdentity", "publishBrowserIdentityMany", "saveAccount", "saveAccountMany", "removeAccount", "removeAccountMany"] as const;

/** The `tilo.accounts` kind at `path`. */
export function accounts(ws: tilo.Workspace, path: string): Accounts {
  return resourceProxy(ws, path, SURFACE, ACCOUNTS_ACTIONS) as unknown as Accounts;
}

