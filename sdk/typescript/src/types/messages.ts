/**
 * Types for Messages (`so.tilo.messages`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Messages` is a `Resource` of the app's `tilo.messages`, made by its factory with the
 * resource's path (`messages/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.messages@add05b84684b0c70";

export interface Conversation {
  with: string;
  messages: Array<tilo.Entry<Message>>;
}

export interface Message {
  from: string;
  to: string;
  body: string;
  date: string;
  at_ms: number;
  outgoing: boolean;
  queued: boolean;
}

export interface AccountSync {
  device: string;
  health: SyncHealth;
  attempted_ms: number;
  succeeded_ms: number | null;
  signed_in_ms: number | null;
  message: string;
}

export interface ListedAccount {
  type_id: string;
  display_name: string;
  config: unknown;
  enabled: boolean;
  tint: number;
  sync: AccountSync | null;
}

/** One of "never", "syncing", "healthy", "offline", "needs_sign_in", "error". */
export type SyncHealth = "never" | "syncing" | "healthy" | "offline" | "needs_sign_in" | "error";

export interface Endpoint {
  service: string;
  recipient: string;
  scope: string;
  revision: string;
  removed: boolean;
}

export interface Person {
  label: string;
  removed: boolean;
  phones: string[];
  emails: string[];
  photo: string;
  photo_size: number;
  endpoints: Array<tilo.Entry<Endpoint>>;
}

export interface Projected {
  person: string;
  label: string;
  phones: string[];
  emails: string[];
  photo: string;
  photo_size: number;
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

/** The arguments of `report-sync`. */
export interface ReportSyncArgs {
  account: string;
  device: string;
  health: SyncHealth;
  at_ms: number;
  signed_in: boolean;
  message: string;
}

/** The arguments of `remove-account`. */
export interface RemoveAccountArgs {
  account: string;
}

/** The arguments of `project-contact`. */
export interface ProjectContactArgs {
  person: string;
  label: string;
}

/** The arguments of `remove-contact`. */
export interface RemoveContactArgs {
  person: string;
}

/** The arguments of `confirm-endpoint`. */
export interface ConfirmEndpointArgs {
  person: string;
  endpoint: string;
  service: string;
  recipient: string;
}

/** The arguments of `confirm-account`. */
export interface ConfirmAccountArgs {
  person: string;
  endpoint: string;
  service: string;
  scope: string;
  recipient: string;
  revision: string;
}

/** The arguments of `replace-endpoint`. */
export interface ReplaceEndpointArgs {
  person: string;
  previous: string;
  endpoint: string;
  service: string;
  recipient: string;
}

/** The arguments of `replace-account`. */
export interface ReplaceAccountArgs {
  person: string;
  previous: string;
  endpoint: string;
  service: string;
  scope: string;
  recipient: string;
  revision: string;
}

/** The arguments of `project-phones`. */
export interface ProjectPhonesArgs {
  person: string;
  phones: string[];
}

/** The arguments of `project-emails`. */
export interface ProjectEmailsArgs {
  person: string;
  emails: string[];
}

/** The arguments of `project-contacts`. */
export interface ProjectContactsArgs {
  people: Projected[];
  removed: string[];
}

/** The arguments of `forget-endpoint`. */
export interface ForgetEndpointArgs {
  person: string;
  endpoint: string;
}

/** Messages — `tilo.messages`. */
export interface Messages extends tilo.Resource {
  readonly conversations: Array<tilo.Entry<Conversation>>;
  deliverMessage(args: {
    from: string;
    body: string;
    to?: string;
    date?: string;
    conversation?: string;
  }): Promise<unknown>;
  sendMessage(args: {
    to: string;
    body: string;
    conversation?: string;
  }): Promise<unknown>;
  reply(args: {
    conversation: string;
    body: string;
  }): Promise<unknown>;
  listGroups(args: {
    service: string;
    account: string;
  }): Promise<void>;
  readGroupMembership(args: {
    service: string;
    account: string;
    group: string;
  }): Promise<void>;
  readGroupHistory(args: {
    service: string;
    account: string;
    group: string;
  }): Promise<void>;
  readHistory(args: {
    choice: string;
  }): Promise<void>;
  readOlderHistory(args: {
    choice: string;
  }): Promise<void>;
  searchMessages(args: {
    query: string;
  }): Promise<void>;
  exportContent(args?: {
    format?: string;
    conversation?: string;
  }): Promise<void>;
}

/** The actions `Messages` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MESSAGES_ACTIONS = ["deliverMessage", "sendMessage", "reply", "listGroups", "readGroupMembership", "readGroupHistory", "readHistory", "readOlderHistory", "searchMessages", "exportContent"] as const;

/** The `tilo.messages` kind at `path`. */
export function messages(ws: tilo.Workspace, path: string): Messages {
  return resourceProxy(ws, path, SURFACE, MESSAGES_ACTIONS) as unknown as Messages;
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
  reportSync(args: {
    account: string;
    device: string;
    health: SyncHealth;
    at_ms: number;
    signed_in: boolean;
    message: string;
  }): Promise<void>;
  /** Runs `report-sync` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  reportSyncMany(items: ReportSyncArgs[]): Promise<tilo.ActionAnswer[]>;
  removeAccount(args: {
    account: string;
  }): Promise<void>;
  /** Runs `remove-account` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  removeAccountMany(items: RemoveAccountArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Accounts` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ACCOUNTS_ACTIONS = ["publishBrowserIdentity", "publishBrowserIdentityMany", "saveAccount", "saveAccountMany", "reportSync", "reportSyncMany", "removeAccount", "removeAccountMany"] as const;

/** The `tilo.accounts` kind at `path`. */
export function accounts(ws: tilo.Workspace, path: string): Accounts {
  return resourceProxy(ws, path, SURFACE, ACCOUNTS_ACTIONS) as unknown as Accounts;
}

/** Contact identities — `tilo.contacts.identities`. */
export interface Identities extends tilo.Resource {
  people: Array<tilo.Entry<Person>>;
  projectContact(args: {
    person: string;
    label: string;
  }): Promise<void>;
  /** Runs `project-contact` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  projectContactMany(items: ProjectContactArgs[]): Promise<tilo.ActionAnswer[]>;
  removeContact(args: {
    person: string;
  }): Promise<void>;
  /** Runs `remove-contact` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  removeContactMany(items: RemoveContactArgs[]): Promise<tilo.ActionAnswer[]>;
  confirmEndpoint(args: {
    person: string;
    endpoint: string;
    service: string;
    recipient: string;
  }): Promise<void>;
  /** Runs `confirm-endpoint` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  confirmEndpointMany(items: ConfirmEndpointArgs[]): Promise<tilo.ActionAnswer[]>;
  confirmAccount(args: {
    person: string;
    endpoint: string;
    service: string;
    scope: string;
    recipient: string;
    revision: string;
  }): Promise<void>;
  /** Runs `confirm-account` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  confirmAccountMany(items: ConfirmAccountArgs[]): Promise<tilo.ActionAnswer[]>;
  replaceEndpoint(args: {
    person: string;
    previous: string;
    endpoint: string;
    service: string;
    recipient: string;
  }): Promise<void>;
  /** Runs `replace-endpoint` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  replaceEndpointMany(items: ReplaceEndpointArgs[]): Promise<tilo.ActionAnswer[]>;
  replaceAccount(args: {
    person: string;
    previous: string;
    endpoint: string;
    service: string;
    scope: string;
    recipient: string;
    revision: string;
  }): Promise<void>;
  /** Runs `replace-account` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  replaceAccountMany(items: ReplaceAccountArgs[]): Promise<tilo.ActionAnswer[]>;
  projectPhones(args: {
    person: string;
    phones: string[];
  }): Promise<void>;
  /** Runs `project-phones` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  projectPhonesMany(items: ProjectPhonesArgs[]): Promise<tilo.ActionAnswer[]>;
  projectEmails(args: {
    person: string;
    emails: string[];
  }): Promise<void>;
  /** Runs `project-emails` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  projectEmailsMany(items: ProjectEmailsArgs[]): Promise<tilo.ActionAnswer[]>;
  projectContacts(args: {
    people: Projected[];
    removed: string[];
  }): Promise<void>;
  /** Runs `project-contacts` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  projectContactsMany(items: ProjectContactsArgs[]): Promise<tilo.ActionAnswer[]>;
  forgetEndpoint(args: {
    person: string;
    endpoint: string;
  }): Promise<void>;
  /** Runs `forget-endpoint` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  forgetEndpointMany(items: ForgetEndpointArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Identities` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const IDENTITIES_ACTIONS = ["projectContact", "projectContactMany", "removeContact", "removeContactMany", "confirmEndpoint", "confirmEndpointMany", "confirmAccount", "confirmAccountMany", "replaceEndpoint", "replaceEndpointMany", "replaceAccount", "replaceAccountMany", "projectPhones", "projectPhonesMany", "projectEmails", "projectEmailsMany", "projectContacts", "projectContactsMany", "forgetEndpoint", "forgetEndpointMany"] as const;

/** The `tilo.contacts.identities` kind at `path`. */
export function identities(ws: tilo.Workspace, path: string): Identities {
  return resourceProxy(ws, path, SURFACE, IDENTITIES_ACTIONS) as unknown as Identities;
}

