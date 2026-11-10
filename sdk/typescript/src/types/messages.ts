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
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "@tilo/sdk";
import type * as tilo from "@tilo/sdk";

export interface Conversation {
  with: string;
  messages: Message[];
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

export interface PersonAccount {
  type_id: string;
  display_name: string;
  config: unknown;
  enabled: boolean;
  tint: number;
}

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
  endpoints: Endpoint[];
}

export interface Projected {
  person: string;
  label: string;
  phones: string[];
  emails: string[];
  photo: string;
}

/** Messages — `tilo.messages`. */
export interface Messages extends tilo.Resource {
  readonly conversations: Conversation[];
  deliverMessage(args: {
    from: string;
    body: string;
    to?: string;
    date?: string;
    conversation?: string;
  }): Promise<unknown | tilo.Run>;
  sendMessage(args: {
    to: string;
    body: string;
    conversation?: string;
  }): Promise<unknown | tilo.Run>;
  reply(args: {
    conversation: string;
    body: string;
  }): Promise<unknown | tilo.Run>;
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

/** The `tilo.messages` kind at `path`. */
export function messages(ws: tilo.Workspace, path: string): Messages {
  return resourceProxy(ws, path) as unknown as Messages;
}

/** Accounts — `tilo.accounts`. */
export interface Accounts extends tilo.Resource {
  accounts: PersonAccount[];
  active: Record<string, string>;
  publishBrowserIdentity(args: {
    root: string;
    origin: string;
    display_name: string;
  }): Promise<tilo.Run | void>;
  saveAccount(args: {
    id: string;
    type_id: string;
    display_name: string;
    config: unknown;
    enabled: boolean;
    tint: number;
  }): Promise<tilo.Run | void>;
  removeAccount(args: {
    account: string;
  }): Promise<tilo.Run | void>;
  setActiveAccount(args: {
    account: string;
  }): Promise<tilo.Run | void>;
}

/** The `tilo.accounts` kind at `path`. */
export function accounts(ws: tilo.Workspace, path: string): Accounts {
  return resourceProxy(ws, path) as unknown as Accounts;
}

/** Contact identities — `tilo.contacts.identities`. */
export interface Identities extends tilo.Resource {
  people: Person[];
  projectContact(args: {
    person: string;
    label: string;
  }): Promise<tilo.Run | void>;
  removeContact(args: {
    person: string;
  }): Promise<tilo.Run | void>;
  confirmEndpoint(args: {
    person: string;
    endpoint: string;
    service: string;
    recipient: string;
  }): Promise<tilo.Run | void>;
  confirmAccount(args: {
    person: string;
    endpoint: string;
    service: string;
    scope: string;
    recipient: string;
    revision: string;
  }): Promise<tilo.Run | void>;
  replaceEndpoint(args: {
    person: string;
    previous: string;
    endpoint: string;
    service: string;
    recipient: string;
  }): Promise<tilo.Run | void>;
  replaceAccount(args: {
    person: string;
    previous: string;
    endpoint: string;
    service: string;
    scope: string;
    recipient: string;
    revision: string;
  }): Promise<tilo.Run | void>;
  projectPhones(args: {
    person: string;
    phones: string[];
  }): Promise<tilo.Run | void>;
  projectEmails(args: {
    person: string;
    emails: string[];
  }): Promise<tilo.Run | void>;
  projectContacts(args: {
    people: Projected[];
    removed: string[];
  }): Promise<tilo.Run | void>;
  forgetEndpoint(args: {
    person: string;
    endpoint: string;
  }): Promise<tilo.Run | void>;
}

/** The `tilo.contacts.identities` kind at `path`. */
export function identities(ws: tilo.Workspace, path: string): Identities {
  return resourceProxy(ws, path) as unknown as Identities;
}

/** Message drafts — `messages.drafts`. */
export interface Drafts extends tilo.Resource {
  drafts: Record<string, string>;
  keepDraft(args: {
    conversation: string;
    body: string;
  }): Promise<tilo.Run | void>;
  dropDraft(args: {
    conversation: string;
  }): Promise<tilo.Run | void>;
}

/** The `messages.drafts` kind at `path`. */
export function drafts(ws: tilo.Workspace, path: string): Drafts {
  return resourceProxy(ws, path) as unknown as Drafts;
}

