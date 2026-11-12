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
 * says where it is.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

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

/** Contacts — `contacts.book`. */
export interface Book extends tilo.Resource {
  contacts: Contact[];
  readonly removed: string[];
  newContact(args: {
    name: string;
  }): Promise<tilo.Run | void>;
  addContacts(args: {
    contacts: Contact[];
  }): Promise<tilo.Run | void>;
  deleteContact(args: {
    contact: string;
  }): Promise<tilo.Run | void>;
  mergeContacts(args: {
    contact: string;
    source: string;
    take_from_source: MergeField[];
  }): Promise<tilo.Run | void>;
  exportVcardContent(args?: {
    contact?: string;
    name?: string;
  }): Promise<void>;
}

/** The `contacts.book` kind at `path`. */
export function book(ws: tilo.Workspace, path: string): Book {
  return resourceProxy(ws, path) as unknown as Book;
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

