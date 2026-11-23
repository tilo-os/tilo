/**
 * Types for Contacts (`so.tilo.contacts`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Labeled` is a `Resource` of the app's `tl.so.tilo.contacts.Labeled`, made by its factory with the
 * resource's path (`contacts/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.contacts@c6768c5776239d02";

/** What a merge may take from the card that goes: the values each card keeps one of. Numbers, mailboxes, addresses and groups always come over; these are the person's to choose. One of "name", "org", "role", "website", "birthday", "notes". */
export type MergeField = "name" | "org" | "role" | "website" | "birthday" | "notes";

/** The arguments of `rename`. */
export interface RenameArgs {
  name: string;
}

/** The arguments of `favorite`. */
export interface FavoriteArgs {
  on?: boolean;
}

/** The arguments of `remove_phone`. */
export interface RemovePhoneArgs {
  phone: Labeled;
}

/** The arguments of `remove_email`. */
export interface RemoveEmailArgs {
  mail: Labeled;
}

/** The arguments of `remove_address`. */
export interface RemoveAddressArgs {
  address: Address;
}

/** The arguments of `ungroup`. */
export interface UngroupArgs {
  name: string;
}

/** The arguments of `group`. */
export interface GroupArgs {
  c: Contact;
  name: string;
}

/** The arguments of `delete`. */
export interface DeleteArgs {
  c: Contact;
}

/** The arguments of `merge_into`. */
export interface MergeIntoArgs {
  keep: Contact;
  goes: Contact;
  take?: MergeField[];
}

/** The arguments of `confirm`. */
export interface ConfirmArgs {
  d: unknown;
  person: unknown;
  service: string;
  scope: string;
  account: string;
}

/** The arguments of `change`. */
export interface ChangeArgs {
  d: unknown;
  person: unknown;
  endpoint: unknown;
  service: string;
  scope: string;
  account: string;
}

/** The arguments of `forget`. */
export interface ForgetArgs {
  d: unknown;
  person: unknown;
  endpoint: unknown;
}

/** The arguments of `share`. */
export interface ShareArgs {
  d: unknown;
  person: unknown;
  c: Contact;
}

/** The arguments of `unshare`. */
export interface UnshareArgs {
  d: unknown;
  person: unknown;
}

/** Labeled — `tl.so.tilo.contacts.Labeled`. */
export interface Labeled extends tilo.Resource {
  fzyd2a52: string;  // label
  b4cilqkx: string;  // value
}

/** The actions `Labeled` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LABELED_ACTIONS = [] as const;

/** The `tl.so.tilo.contacts.Labeled` kind at `path`. */
export function labeled(ws: tilo.Workspace, path: string): Labeled {
  return resourceProxy(ws, path, SURFACE, LABELED_ACTIONS) as unknown as Labeled;
}

/** Address — `tl.so.tilo.contacts.Address`. */
export interface Address extends tilo.Resource {
  "5iipld72": string;  // label
  zpxsouby: string;  // street
  txsphfy2: string;  // city
  g55zwvsh: string;  // region
  xuayok2l: string;  // postal
  xlbwdlve: string;  // country
}

/** The actions `Address` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ADDRESS_ACTIONS = [] as const;

/** The `tl.so.tilo.contacts.Address` kind at `path`. */
export function address(ws: tilo.Workspace, path: string): Address {
  return resourceProxy(ws, path, SURFACE, ADDRESS_ACTIONS) as unknown as Address;
}

/** Contact — `tl.so.tilo.contacts.Contact`. */
export interface Contact extends tilo.Resource {
  zut4kvim: string;  // name
  rfhxkccf: string;  // org
  "7wq5grnt": string;  // role
  dg7b4wfh: string;  // website
  fc62qk5v: string | null;  // birthday
  "4lfazbdq": string;  // notes
  lurugxra: Array<tilo.Entry<Labeled>>;  // phones
  hfbs4gxe: Array<tilo.Entry<Labeled>>;  // emails
  orxuftuv: Array<tilo.Entry<Address>>;  // addresses
  vea52w55: string[];  // groups
  ur3qshor: boolean;  // favorite
  kvhikhu4: string;  // photo
  /** Renames a contact. */
  rename(args: {
    name: string;
  }): Promise<void>;
  /** Runs `rename` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  renameMany(items: RenameArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds a contact to favorites, or takes them off. */
  favorite(args?: {
    on?: boolean;
  }): Promise<void>;
  /** Runs `favorite` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  favoriteMany(items: FavoriteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Gives a contact a phone number, as the number `number` and the kind `label` where the caller knows them already. Addressed from the card's own button, both are empty for the person to fill in. */
  add_phone(args?: {
    number?: string;
    label?: string;
  }): Promise<Labeled>;
  /** Takes one of a contact's phone numbers away. */
  remove_phone(args: {
    phone: Labeled;
  }): Promise<void>;
  /** Runs `remove_phone` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_phoneMany(items: RemovePhoneArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Gives a contact a mailbox, as the `address` and the kind `label` where the caller knows them already. Addressed from the card's own button, both are empty for the person to fill in. */
  add_email(args?: {
    address?: string;
    label?: string;
  }): Promise<Labeled>;
  /** Takes one of a contact's mailboxes away. */
  remove_email(args: {
    mail: Labeled;
  }): Promise<void>;
  /** Runs `remove_email` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_emailMany(items: RemoveEmailArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Gives a contact a postal address to fill in. */
  add_address(): Promise<Address>;
  /** Takes one of a contact's addresses away. */
  remove_address(args: {
    address: Address;
  }): Promise<void>;
  /** Runs `remove_address` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_addressMany(items: RemoveAddressArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes a contact out of a group. */
  ungroup(args: {
    name: string;
  }): Promise<void>;
  /** Runs `ungroup` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  ungroupMany(items: UngroupArgs[]): Promise<tilo.ActionAnswer[]>;
  /** A contact as a file: their card, named for them. */
  export_contact(): Promise<unknown>;
}

/** The actions `Contact` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CONTACT_ACTIONS = ["rename", "renameMany", "favorite", "favoriteMany", "add_phone", "remove_phone", "remove_phoneMany", "add_email", "remove_email", "remove_emailMany", "add_address", "remove_address", "remove_addressMany", "ungroup", "ungroupMany", "export_contact"] as const;

/** The `tl.so.tilo.contacts.Contact` kind at `path`. */
export function contact(ws: tilo.Workspace, path: string): Contact {
  return resourceProxy(ws, path, SURFACE, CONTACT_ACTIONS) as unknown as Contact;
}

/** Group — `tl.so.tilo.contacts.Group`. */
export interface Group extends tilo.Resource {
  jn6l7z6v: string;  // name
}

/** The actions `Group` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const GROUP_ACTIONS = [] as const;

/** The `tl.so.tilo.contacts.Group` kind at `path`. */
export function group(ws: tilo.Workspace, path: string): Group {
  return resourceProxy(ws, path, SURFACE, GROUP_ACTIONS) as unknown as Group;
}

/** Book — `tl.so.tilo.contacts.Book`. */
export interface Book extends tilo.Resource {
  qfj7l6eu: Array<tilo.Entry<Contact>>;  // contacts
  uyyz5pg2: Array<tilo.Entry<Group>>;  // groups
  /** A new contact, named by what is typed where they were made; a book's entry gives the name it takes, so nothing is left called 'No name yet' by a person who typed one. */
  new_contact(args?: {
    name?: string;
  }): Promise<Contact>;
  /** Puts a contact in a group, making the group the book's place if it is new: the same group however it is spelled, so 'Work' and 'work' are one place. */
  group(args: {
    c: Contact;
    name: string;
  }): Promise<void>;
  /** Runs `group` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  groupMany(items: GroupArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Starts a group the book offers as a place, with nobody in it yet. */
  new_group(args: {
    name: string;
  }): Promise<Group>;
  /** Deletes a contact from the book. */
  delete(args: {
    c: Contact;
  }): Promise<void>;
  /** Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteMany(items: DeleteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Merges the card that goes into the one that stays: every number, mailbox, address and group of both is the card that stays, the fields `take` names come from the card that goes, and the card that goes is deleted. One change, so undo brings both cards back. */
  merge_into(args: {
    keep: Contact;
    goes: Contact;
    take?: MergeField[];
  }): Promise<void>;
  /** Runs `merge_into` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  merge_intoMany(items: MergeIntoArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Every contact as one vCard file, which another address book reads whole. */
  export_book(): Promise<unknown>;
  /** Imports the contacts a vCard file holds into the book, each card a contact of its own, and answers how many were added. A file whose cards cannot be read adds nothing and changes nothing: what it says is the refusal. */
  import_vcard(args: {
    file: unknown;
  }): Promise<number>;
  /** The cards `cards`, the words of a vCard file, as contacts of this book, and how many named somebody: what an importer, an agent or a script hands a book in one call. */
  add_contacts(args: {
    cards: string;
  }): Promise<number>;
  /** The contact a vCard card says, made in the book, or none for a card that names nobody. Its rows are filled after it stands in the book, as every resource's are. */
  add_card(args: {
    card: string;
  }): Promise<Contact | null>;
  /** The contact the book holds for `address`, as an action: what a person, an agent or another app asks before they write to it. Nothing is changed and nothing is guessed from a name. */
  find_for_address(args: {
    address: string;
  }): Promise<Contact | null>;
}

/** The actions `Book` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const BOOK_ACTIONS = ["new_contact", "group", "groupMany", "new_group", "delete", "deleteMany", "merge_into", "merge_intoMany", "export_book", "import_vcard", "add_contacts", "add_card", "find_for_address"] as const;

/** The `tl.so.tilo.contacts.Book` kind at `path`. */
export function book(ws: tilo.Workspace, path: string): Book {
  return resourceProxy(ws, path, SURFACE, BOOK_ACTIONS) as unknown as Book;
}

/** Contacts — `tl.so.tilo.contacts`. */
export interface Contacts extends tilo.Resource {
  /** Binds `account` of `service` to the person: the directory's `confirm-endpoint`, or `confirm-account` where the service names an account scope. */
  confirm(args: {
    d: unknown;
    person: unknown;
    service: string;
    scope: string;
    account: string;
  }): Promise<void>;
  /** Runs `confirm` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  confirmMany(items: ConfirmArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Changes the account an association names, under the directory's rule that a changed address needs review before anything is sent to it. */
  change(args: {
    d: unknown;
    person: unknown;
    endpoint: unknown;
    service: string;
    scope: string;
    account: string;
  }): Promise<void>;
  /** Runs `change` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  changeMany(items: ChangeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes an association away: messages from that account stop reaching this contact, and what was already received is unchanged. */
  forget(args: {
    d: unknown;
    person: unknown;
    endpoint: unknown;
  }): Promise<void>;
  /** Runs `forget` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  forgetMany(items: ForgetArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shares this card with the workspace's apps, under the person it is: the name, the numbers and the mailboxes another app reads of a contact before it writes to them. */
  share(args: {
    d: unknown;
    person: unknown;
    c: Contact;
  }): Promise<void>;
  /** Runs `share` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  shareMany(items: ShareArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes the contact out of the directory, leaving its card in the book: what the apps read of it stops naming it. */
  unshare(args: {
    d: unknown;
    person: unknown;
  }): Promise<void>;
  /** Runs `unshare` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  unshareMany(items: UnshareArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Contacts` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CONTACTS_ACTIONS = ["confirm", "confirmMany", "change", "changeMany", "forget", "forgetMany", "share", "shareMany", "unshare", "unshareMany"] as const;

/** The `tl.so.tilo.contacts` kind at `path`. */
export function contacts(ws: tilo.Workspace, path: string): Contacts {
  return resourceProxy(ws, path, SURFACE, CONTACTS_ACTIONS) as unknown as Contacts;
}

