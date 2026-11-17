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
 * says where it is. An undoable action that answers nothing also has a `<action>Many`,
which applies it to every argument object of a list in a few all-or-nothing steps,
all of them one run.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** The version of the surface these types were made from: a workspace whose app
 * publishes another refuses their actions rather than run what they no longer
 * describe. */
export const SURFACE = "so.tilo.mail@3de88f51444d06e9";

export interface Delivery {
  from: string;
  to: string | null;
  cc: string | null;
  subject: string;
  body: string;
  date: string | null;
  folder: Folder | null;
  read: boolean | null;
  reply_to: string | null;
  conversation: string | null;
  message_id: string | null;
  original: unknown | null;
}

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
  original: unknown | null;
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
  endpoints: Array<tilo.Entry<Endpoint>>;
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

/** The arguments of `set-active-account`. */
export interface SetActiveAccountArgs {
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

/** The arguments of `trust`. */
export interface TrustArgs {
  sender: string;
}

/** The arguments of `distrust`. */
export interface DistrustArgs {
  sender: string;
}

/** The arguments of `keep-copies`. */
export interface KeepCopiesArgs {
  copies: NewCopy[];
}

/** The arguments of `forget-copies`. */
export interface ForgetCopiesArgs {
  messages: string[];
}

/** Mail — `mail.mailbox`. */
export interface Mailbox extends tilo.Resource {
  messages: Array<tilo.Entry<MessageState>>;
  folders: Array<tilo.Entry<LocalFolder>>;
  draft: DraftState;
  drafts: Array<tilo.Entry<DraftState>>;
  held: Array<tilo.Entry<HeldMessage>>;
  markRead(args: {
    messages: string[];
    read: boolean;
  }): Promise<void>;
  archive(args: {
    messages: string[];
  }): Promise<void>;
  trash(args: {
    messages: string[];
  }): Promise<void>;
  moveMessages(args: {
    messages: string[];
    to: Folder;
  }): Promise<void>;
  purge(args: {
    messages: string[];
  }): Promise<void>;
  ingest(args: {
    messages: IngestBaseline[];
  }): Promise<void>;
  serverApplied(args: {
    update: ServerUpdate;
  }): Promise<void>;
  startDraftBody(args: {
    text: string;
    html?: string;
    draft?: string;
  }): Promise<void>;
  saveDraft(args: {
    message: DraftData;
    draft?: string;
  }): Promise<void>;
  createDraft(args: {
    id: string;
    message: DraftData;
    in_reply_to?: string;
    notes?: string;
  }): Promise<unknown>;
  showDraft(args: {
    message: string;
    draft?: string;
  }): Promise<void>;
  attach(args: {
    content: string;
    name: string;
    size?: number;
    mime?: string;
    draft?: string;
  }): Promise<void>;
  discardDraft(args?: {
    draft?: string;
  }): Promise<void>;
  sendMessage(args: {
    account: string;
    message: DraftData;
    in_reply_to?: string;
    draft?: string;
  }): Promise<unknown>;
  checkMail(args: {
    account: string;
    folder: Folder;
  }): Promise<unknown>;
  deliverMessage(args: {
    from: string;
    subject: string;
    body: string;
    to?: string;
    cc?: string;
    date?: string;
    folder?: Folder;
    read?: boolean;
    reply_to?: string;
    conversation?: string;
    message_id?: string;
    original?: unknown;
  }): Promise<unknown>;
  draftMessage(args: {
    to: string;
    subject: string;
    body: string;
    cc?: string;
    notes?: string;
  }): Promise<unknown>;
  cancelSend(args: {
    id: string;
  }): Promise<unknown>;
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

/** The actions `Mailbox` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MAILBOX_ACTIONS = ["markRead", "archive", "trash", "moveMessages", "purge", "ingest", "serverApplied", "startDraftBody", "saveDraft", "createDraft", "showDraft", "attach", "discardDraft", "sendMessage", "checkMail", "deliverMessage", "draftMessage", "cancelSend", "searchMessages", "exportContent"] as const;

/** The `mail.mailbox` kind at `path`. */
export function mailbox(ws: tilo.Workspace, path: string): Mailbox {
  return resourceProxy(ws, path, SURFACE, MAILBOX_ACTIONS) as unknown as Mailbox;
}

/** Accounts — `tilo.accounts`. */
export interface Accounts extends tilo.Resource {
  accounts: Array<tilo.Entry<PersonAccount>>;
  active: Record<string, string>;
  publishBrowserIdentity(args: {
    root: string;
    origin: string;
    display_name: string;
  }): Promise<void>;
  /** Runs `publish-browser-identity` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  publishBrowserIdentityMany(items: PublishBrowserIdentityArgs[]): Promise<tilo.ActionAnswer[]>;
  saveAccount(args: {
    id: string;
    type_id: string;
    display_name: string;
    config: unknown;
    enabled: boolean;
    tint: number;
  }): Promise<void>;
  /** Runs `save-account` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  saveAccountMany(items: SaveAccountArgs[]): Promise<tilo.ActionAnswer[]>;
  removeAccount(args: {
    account: string;
  }): Promise<void>;
  /** Runs `remove-account` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  removeAccountMany(items: RemoveAccountArgs[]): Promise<tilo.ActionAnswer[]>;
  setActiveAccount(args: {
    account: string;
  }): Promise<void>;
  /** Runs `set-active-account` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  setActiveAccountMany(items: SetActiveAccountArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Accounts` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ACCOUNTS_ACTIONS = ["publishBrowserIdentity", "publishBrowserIdentityMany", "saveAccount", "saveAccountMany", "removeAccount", "removeAccountMany", "setActiveAccount", "setActiveAccountMany"] as const;

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
  /** Runs `project-contact` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  projectContactMany(items: ProjectContactArgs[]): Promise<tilo.ActionAnswer[]>;
  removeContact(args: {
    person: string;
  }): Promise<void>;
  /** Runs `remove-contact` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  removeContactMany(items: RemoveContactArgs[]): Promise<tilo.ActionAnswer[]>;
  confirmEndpoint(args: {
    person: string;
    endpoint: string;
    service: string;
    recipient: string;
  }): Promise<void>;
  /** Runs `confirm-endpoint` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  confirmEndpointMany(items: ConfirmEndpointArgs[]): Promise<tilo.ActionAnswer[]>;
  confirmAccount(args: {
    person: string;
    endpoint: string;
    service: string;
    scope: string;
    recipient: string;
    revision: string;
  }): Promise<void>;
  /** Runs `confirm-account` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  confirmAccountMany(items: ConfirmAccountArgs[]): Promise<tilo.ActionAnswer[]>;
  replaceEndpoint(args: {
    person: string;
    previous: string;
    endpoint: string;
    service: string;
    recipient: string;
  }): Promise<void>;
  /** Runs `replace-endpoint` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
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
  /** Runs `replace-account` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  replaceAccountMany(items: ReplaceAccountArgs[]): Promise<tilo.ActionAnswer[]>;
  projectPhones(args: {
    person: string;
    phones: string[];
  }): Promise<void>;
  /** Runs `project-phones` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  projectPhonesMany(items: ProjectPhonesArgs[]): Promise<tilo.ActionAnswer[]>;
  projectEmails(args: {
    person: string;
    emails: string[];
  }): Promise<void>;
  /** Runs `project-emails` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  projectEmailsMany(items: ProjectEmailsArgs[]): Promise<tilo.ActionAnswer[]>;
  projectContacts(args: {
    people: Projected[];
    removed: string[];
  }): Promise<void>;
  /** Runs `project-contacts` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  projectContactsMany(items: ProjectContactsArgs[]): Promise<tilo.ActionAnswer[]>;
  forgetEndpoint(args: {
    person: string;
    endpoint: string;
  }): Promise<void>;
  /** Runs `forget-endpoint` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  forgetEndpointMany(items: ForgetEndpointArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Identities` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const IDENTITIES_ACTIONS = ["projectContact", "projectContactMany", "removeContact", "removeContactMany", "confirmEndpoint", "confirmEndpointMany", "confirmAccount", "confirmAccountMany", "replaceEndpoint", "replaceEndpointMany", "replaceAccount", "replaceAccountMany", "projectPhones", "projectPhonesMany", "projectEmails", "projectEmailsMany", "projectContacts", "projectContactsMany", "forgetEndpoint", "forgetEndpointMany"] as const;

/** The `tilo.contacts.identities` kind at `path`. */
export function identities(ws: tilo.Workspace, path: string): Identities {
  return resourceProxy(ws, path, SURFACE, IDENTITIES_ACTIONS) as unknown as Identities;
}

/** Trusted senders — `mail.trusted-senders`. */
export interface TrustedSenders extends tilo.Resource {
  senders: string[];
  trust(args: {
    sender: string;
  }): Promise<void>;
  /** Runs `trust` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  trustMany(items: TrustArgs[]): Promise<tilo.ActionAnswer[]>;
  distrust(args: {
    sender: string;
  }): Promise<void>;
  /** Runs `distrust` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  distrustMany(items: DistrustArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `TrustedSenders` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TRUSTEDSENDERS_ACTIONS = ["trust", "trustMany", "distrust", "distrustMany"] as const;

/** The `mail.trusted-senders` kind at `path`. */
export function trustedSenders(ws: tilo.Workspace, path: string): TrustedSenders {
  return resourceProxy(ws, path, SURFACE, TRUSTEDSENDERS_ACTIONS) as unknown as TrustedSenders;
}

/** Mail on your devices — `mail.copies`. */
export interface Copies extends tilo.Resource {
  readonly copies: Array<tilo.Entry<KeptCopy>>;
  keepCopies(args: {
    copies: NewCopy[];
  }): Promise<void>;
  /** Runs `keep-copies` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  keepCopiesMany(items: KeepCopiesArgs[]): Promise<tilo.ActionAnswer[]>;
  forgetCopies(args: {
    messages: string[];
  }): Promise<void>;
  /** Runs `forget-copies` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  forgetCopiesMany(items: ForgetCopiesArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Copies` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const COPIES_ACTIONS = ["keepCopies", "keepCopiesMany", "forgetCopies", "forgetCopiesMany"] as const;

/** The `mail.copies` kind at `path`. */
export function copies(ws: tilo.Workspace, path: string): Copies {
  return resourceProxy(ws, path, SURFACE, COPIES_ACTIONS) as unknown as Copies;
}

