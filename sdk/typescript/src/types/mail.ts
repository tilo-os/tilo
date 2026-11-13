/**
 * Types for Mail (`so.tilo.mail`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Mailbox` is a `Resource` of the app's `mail.mailbox`, made by its factory with the
 * resource's path (`mail/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.mail@c0f0bb93ba1205e5";

export interface DraftAttachment {
  content_id: string;
  filename: string;
  mime: string;
  size: number;
}

export interface DraftCopy {
  account: string;
  locator: string;
  written: string;
}

export interface DraftData {
  sender: DraftSender | null;
  to: string;
  cc: string;
  bcc: string;
  subject: string;
  body: string;
  html_body: string | null;
  calendar_ics: string | null;
  attachments: DraftAttachment[];
}

export interface DraftSender {
  account: string;
  email: string;
}

export interface DraftState {
  sender: unknown | null;
  to: string;
  cc: string;
  bcc: string;
  subject: string;
  body: string;
  html_body: string | null;
  calendar_ics: string | null;
  attachments: unknown[];
  rich_body: string | null;
  shown_as: string | null;
  drafted: string | null;
  in_reply_to: string | null;
  notes: string;
  server: DraftCopy | null;
}

/** One of "inbox", "starred", "sent", "drafts", "archive", "trash", "junk". */
export type Folder = "inbox" | "starred" | "sent" | "drafts" | "archive" | "trash" | "junk";

export interface HeldMessage {
  message_id: string;
  from: string;
  to: string;
  cc: string;
  subject: string;
  body: string;
  minutes: number;
  in_reply_to: string | null;
  conversation: string;
}

export interface IngestBaseline {
  id: string;
  read: boolean | null;
  starred: boolean | null;
  folder: string | null;
  account: string | null;
  locator: string | null;
  label: string | null;
}

export interface LocalFolder {
  name: string;
}

export interface MessageState {
  read: boolean;
  starred: boolean;
  folder: Folder;
  labels: string[];
  server: ServerCopy | null;
  purged: boolean;
  label: string;
}

export interface ServerCopy {
  account: string;
  locator: string;
  read: boolean;
  starred: boolean;
  folder: Folder;
  removed: boolean;
}

/** One of Flags, Move, Remove, SaveDraft: an object whose case the app tags it with. */
export type ServerUpdate = Record<string, unknown>;

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

export interface KeptCopy {
  account: string;
  content: string;
  minutes: number;
  folder: Folder;
  uid: number;
  uid_validity: number;
}

export interface NewCopy {
  id: string;
  account: string;
  content: string;
  minutes: number;
  folder: Folder;
  uid: number;
  uid_validity: number;
}

/** Mail — `mail.mailbox`. */
export interface Mailbox extends tilo.Resource {
  messages: MessageState[];
  folders: LocalFolder[];
  draft: DraftState;
  drafts: DraftState[];
  held: HeldMessage[];
  markRead(args: {
    messages: string[];
    read: boolean;
  }): Promise<tilo.Run | void>;
  archive(args: {
    messages: string[];
  }): Promise<tilo.Run | void>;
  trash(args: {
    messages: string[];
  }): Promise<tilo.Run | void>;
  moveMessages(args: {
    messages: string[];
    to: Folder;
  }): Promise<tilo.Run | void>;
  purge(args: {
    messages: string[];
  }): Promise<tilo.Run | void>;
  ingest(args: {
    messages: IngestBaseline[];
  }): Promise<tilo.Run | void>;
  serverApplied(args: {
    update: ServerUpdate;
  }): Promise<tilo.Run | void>;
  startDraftBody(args: {
    text: string;
    html?: string;
    draft?: string;
  }): Promise<tilo.Run | void>;
  saveDraft(args: {
    message: DraftData;
    draft?: string;
  }): Promise<tilo.Run | void>;
  createDraft(args: {
    id: string;
    message: DraftData;
    in_reply_to?: string;
    notes?: string;
  }): Promise<unknown | tilo.Run>;
  showDraft(args: {
    message: string;
    draft?: string;
  }): Promise<tilo.Run | void>;
  attach(args: {
    content: string;
    name: string;
    size?: number;
    mime?: string;
    draft?: string;
  }): Promise<tilo.Run | void>;
  discardDraft(args?: {
    draft?: string;
  }): Promise<tilo.Run | void>;
  sendMessage(args: {
    account: string;
    message: DraftData;
    in_reply_to?: string;
    draft?: string;
  }): Promise<unknown | tilo.Run>;
  deliverMessage(args: {
    from: string;
    subject: string;
    body: string;
    to?: string;
    cc?: string;
    date?: string;
    folder?: unknown;
    read?: boolean;
    reply_to?: string;
    conversation?: string;
  }): Promise<unknown | tilo.Run>;
  draftMessage(args: {
    to: string;
    subject: string;
    body: string;
    cc?: string;
    notes?: string;
  }): Promise<unknown | tilo.Run>;
  cancelSend(args: {
    id: string;
  }): Promise<unknown | tilo.Run>;
  searchMessages(args?: {
    query?: string;
    folder?: Folder;
    from?: string;
    subject?: string;
    since?: string;
    until?: string;
    has_attachment?: boolean;
  }): Promise<void>;
  exportContent(args?: {
    format?: string;
    message?: string;
    folder?: Folder;
  }): Promise<void>;
}

/** The `mail.mailbox` kind at `path`. */
export function mailbox(ws: tilo.Workspace, path: string): Mailbox {
  return resourceProxy(ws, path, SURFACE) as unknown as Mailbox;
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
  return resourceProxy(ws, path, SURFACE) as unknown as Accounts;
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
  return resourceProxy(ws, path, SURFACE) as unknown as Identities;
}

/** Trusted senders — `mail.trusted-senders`. */
export interface TrustedSenders extends tilo.Resource {
  senders: string[];
  trust(args: {
    sender: string;
  }): Promise<tilo.Run | void>;
  distrust(args: {
    sender: string;
  }): Promise<tilo.Run | void>;
}

/** The `mail.trusted-senders` kind at `path`. */
export function trustedSenders(ws: tilo.Workspace, path: string): TrustedSenders {
  return resourceProxy(ws, path, SURFACE) as unknown as TrustedSenders;
}

/** Mail on your devices — `mail.copies`. */
export interface Copies extends tilo.Resource {
  readonly copies: KeptCopy[];
  keepCopies(args: {
    copies: NewCopy[];
  }): Promise<tilo.Run | void>;
  forgetCopies(args: {
    messages: string[];
  }): Promise<tilo.Run | void>;
}

/** The `mail.copies` kind at `path`. */
export function copies(ws: tilo.Workspace, path: string): Copies {
  return resourceProxy(ws, path, SURFACE) as unknown as Copies;
}

