/**
 * Types for Contacts (`so.tilo.contacts`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Book` is a `Resource` of the app's `contacts.book`, made by its factory with the
 * resource's path (`contacts/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.contacts@862e8f23951f9266";

export interface Address {
  id: string;
  label: string;
  street: string;
  city: string;
  region: string;
  postal: string;
  country: string;
}

export interface Contact {
  id: string;
  first: string;
  last: string;
  org: string;
  title: string;
  website: string;
  birthday: unknown | null;
  notes: string;
  phones: Labeled[];
  emails: Labeled[];
  addresses: Address[];
  groups: GroupEntry[];
  favorite: boolean;
  photo: string;
}

export interface GroupEntry {
  id: string;
  value: string;
}

export interface Labeled {
  id: string;
  label: string;
  value: string;
}

/** One of "First", "Last", "Organization", "Title", "Website", "Birthday", "Notes". */
export type MergeField = "First" | "Last" | "Organization" | "Title" | "Website" | "Birthday" | "Notes";

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

/** The arguments of `new-contact`. */
export interface NewContactArgs {
  name: string;
}

/** The arguments of `add-contacts`. */
export interface AddContactsArgs {
  contacts: Contact[];
}

/** The arguments of `delete-contact`. */
export interface DeleteContactArgs {
  contact: string;
}

/** The arguments of `merge-contacts`. */
export interface MergeContactsArgs {
  contact: string;
  source: string;
  take_from_source: MergeField[];
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

/** Contacts — `contacts.book`. */
export interface Book extends tilo.Resource {
  contacts: Contact[];
  readonly removed: string[];
  newContact(args: {
    name: string;
  }): Promise<tilo.Run | void>;
  /** Runs `new-contact` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  newContactMany(items: NewContactArgs[]): Promise<tilo.ActionAnswer[]>;
  addContacts(args: {
    contacts: Contact[];
  }): Promise<tilo.Run | void>;
  /** Runs `add-contacts` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  addContactsMany(items: AddContactsArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteContact(args: {
    contact: string;
  }): Promise<tilo.Run | void>;
  /** Runs `delete-contact` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  deleteContactMany(items: DeleteContactArgs[]): Promise<tilo.ActionAnswer[]>;
  mergeContacts(args: {
    contact: string;
    source: string;
    take_from_source: MergeField[];
  }): Promise<tilo.Run | void>;
  /** Runs `merge-contacts` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  mergeContactsMany(items: MergeContactsArgs[]): Promise<tilo.ActionAnswer[]>;
  exportVcardContent(args?: {
    contact?: string;
    name?: string;
  }): Promise<void>;
}

/** The `contacts.book` kind at `path`. */
export function book(ws: tilo.Workspace, path: string): Book {
  return resourceProxy(ws, path, SURFACE) as unknown as Book;
}

/** Contact identities — `tilo.contacts.identities`. */
export interface Identities extends tilo.Resource {
  people: Person[];
  projectContact(args: {
    person: string;
    label: string;
  }): Promise<tilo.Run | void>;
  /** Runs `project-contact` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  projectContactMany(items: ProjectContactArgs[]): Promise<tilo.ActionAnswer[]>;
  removeContact(args: {
    person: string;
  }): Promise<tilo.Run | void>;
  /** Runs `remove-contact` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  removeContactMany(items: RemoveContactArgs[]): Promise<tilo.ActionAnswer[]>;
  confirmEndpoint(args: {
    person: string;
    endpoint: string;
    service: string;
    recipient: string;
  }): Promise<tilo.Run | void>;
  /** Runs `confirm-endpoint` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  confirmEndpointMany(items: ConfirmEndpointArgs[]): Promise<tilo.ActionAnswer[]>;
  confirmAccount(args: {
    person: string;
    endpoint: string;
    service: string;
    scope: string;
    recipient: string;
    revision: string;
  }): Promise<tilo.Run | void>;
  /** Runs `confirm-account` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  confirmAccountMany(items: ConfirmAccountArgs[]): Promise<tilo.ActionAnswer[]>;
  replaceEndpoint(args: {
    person: string;
    previous: string;
    endpoint: string;
    service: string;
    recipient: string;
  }): Promise<tilo.Run | void>;
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
  }): Promise<tilo.Run | void>;
  /** Runs `replace-account` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  replaceAccountMany(items: ReplaceAccountArgs[]): Promise<tilo.ActionAnswer[]>;
  projectPhones(args: {
    person: string;
    phones: string[];
  }): Promise<tilo.Run | void>;
  /** Runs `project-phones` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  projectPhonesMany(items: ProjectPhonesArgs[]): Promise<tilo.ActionAnswer[]>;
  projectEmails(args: {
    person: string;
    emails: string[];
  }): Promise<tilo.Run | void>;
  /** Runs `project-emails` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  projectEmailsMany(items: ProjectEmailsArgs[]): Promise<tilo.ActionAnswer[]>;
  projectContacts(args: {
    people: Projected[];
    removed: string[];
  }): Promise<tilo.Run | void>;
  /** Runs `project-contacts` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  projectContactsMany(items: ProjectContactsArgs[]): Promise<tilo.ActionAnswer[]>;
  forgetEndpoint(args: {
    person: string;
    endpoint: string;
  }): Promise<tilo.Run | void>;
  /** Runs `forget-endpoint` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  forgetEndpointMany(items: ForgetEndpointArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The `tilo.contacts.identities` kind at `path`. */
export function identities(ws: tilo.Workspace, path: string): Identities {
  return resourceProxy(ws, path, SURFACE) as unknown as Identities;
}

