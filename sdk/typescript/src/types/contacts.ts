/**
 * Types for Contacts (`so.tilo.contacts`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
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
export const SURFACE = "so.tilo.contacts@c32a407883c8217b";

/** The versions of the card format another address book reads. One of "v4", "v3". */
export type VCardVersion = "v4" | "v3";

/** What one card of a file is to the book. One of "taken", "held", "review", "skipped". */
export type CardFate = "taken" | "held" | "review" | "skipped";

/** What a merge may take from the card that goes: the values each card keeps one of. Numbers, mailboxes, addresses and groups always come over; these are the person's to choose. One of "name", "org", "role", "website", "birthday", "notes". */
export type MergeField = "name" | "org" | "role" | "website" | "birthday" | "notes";

/** The arguments of `edit-block`. */
export interface EditBlockArgs {
  block: string;
  text: string;
}

/** The arguments of `insert-block`. */
export interface InsertBlockArgs {
  block: string;
  text: string;
}

/** The arguments of `delete-block`. */
export interface DeleteBlockArgs {
  block: string;
}

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

/** The arguments of `keep_birthday_year`. */
export interface KeepBirthdayYearArgs {
  on: boolean;
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
  directory?: unknown;
}

/** The arguments of `merge_into`. */
export interface MergeIntoArgs {
  keep: Contact;
  goes: Contact;
  take?: MergeField[];
  directory?: unknown;
}

/** The arguments of `dismiss_import`. */
export interface DismissImportArgs {
}

/** The arguments of `keep_existing`. */
export interface KeepExistingArgs {
  p: ImportCard;
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

/** The arguments of `share_card`. */
export interface ShareCardArgs {
  d: unknown;
  c: Contact;
}

/** The arguments of `unshare`. */
export interface UnshareArgs {
  d: unknown;
  person: unknown;
  c: Contact;
}

/** Notice — `tl.tilo.host.Notice`. */
export interface Notice extends tilo.Resource {
  notice_to: string;  // to
  notice_title: string;  // title
  notice_body: string;  // body
  notice_opens: unknown;  // opens
  notice_seen: boolean;  // seen
  notice_device: string;  // device
  notice_actions: string[];  // actions
}

/** The actions `Notice` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const NOTICE_ACTIONS = [] as const;

/** The `tl.tilo.host.Notice` kind at `path`. */
export function notice(ws: tilo.Workspace, path: string): Notice {
  return resourceProxy(ws, path, SURFACE, NOTICE_ACTIONS) as unknown as Notice;
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
  "32tuvb6l": boolean;  // birthday_year
  "4lfazbdq": string;  // notes
  lurugxra: Array<tilo.Entry<Labeled>>;  // phones
  hfbs4gxe: Array<tilo.Entry<Labeled>>;  // emails
  orxuftuv: Array<tilo.Entry<Address>>;  // addresses
  vea52w55: string[];  // groups
  ur3qshor: boolean;  // favorite
  kvhikhu4: string;  // photo
  szrijd7k: string;  // person
  q3um3t7j: string;  // uid
  /** Set one paragraph of a rich-text field to what a Markdown text says. */
  editBlock(args: {
    block: string;
    text: string;
  }): Promise<void>;
  /** Runs `edit-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  editBlockMany(items: EditBlockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Put a paragraph before the one the address names. */
  insertBlock(args: {
    block: string;
    text: string;
  }): Promise<void>;
  /** Runs `insert-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  insertBlockMany(items: InsertBlockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Remove the paragraph the address names, and the paragraphs it holds. */
  deleteBlock(args: {
    block: string;
  }): Promise<void>;
  /** Runs `delete-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteBlockMany(items: DeleteBlockArgs[]): Promise<tilo.ActionAnswer[]>;
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
  /** Keeps a birthday with its year or without it: a birthday kept without one is the day it falls on every year, as many address books keep one. */
  keep_birthday_year(args: {
    on: boolean;
  }): Promise<void>;
  /** Runs `keep_birthday_year` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  keep_birthday_yearMany(items: KeepBirthdayYearArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes a contact out of a group. */
  ungroup(args: {
    name: string;
  }): Promise<void>;
  /** Runs `ungroup` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  ungroupMany(items: UngroupArgs[]): Promise<tilo.ActionAnswer[]>;
  /** A contact as a file: their card, named for them. The system's one Export… lists it and saves it where the person says. */
  export_contact(args?: {
    version?: VCardVersion;
  }): Promise<unknown>;
}

/** The actions `Contact` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CONTACT_ACTIONS = ["editBlock", "editBlockMany", "insertBlock", "insertBlockMany", "deleteBlock", "deleteBlockMany", "rename", "renameMany", "favorite", "favoriteMany", "add_phone", "remove_phone", "remove_phoneMany", "add_email", "remove_email", "remove_emailMany", "add_address", "remove_address", "remove_addressMany", "keep_birthday_year", "keep_birthday_yearMany", "ungroup", "ungroupMany", "export_contact"] as const;

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

/** ImportCard — `tl.so.tilo.contacts.ImportCard`. */
export interface ImportCard extends tilo.Resource {
  op5ercnd: string;  // card
  a4s7qvbf: string;  // name
  kcnlll33: string;  // uid
  rsywb3bl: Contact | null;  // existing
}

/** The actions `ImportCard` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const IMPORTCARD_ACTIONS = [] as const;

/** The `tl.so.tilo.contacts.ImportCard` kind at `path`. */
export function importCard(ws: tilo.Workspace, path: string): ImportCard {
  return resourceProxy(ws, path, SURFACE, IMPORTCARD_ACTIONS) as unknown as ImportCard;
}

/** ImportReport — `tl.so.tilo.contacts.ImportReport`. */
export interface ImportReport extends tilo.Resource {
  ydus2juj: number;  // imported
  bqy264j6: number;  // unchanged
  jm2un2fr: number;  // awaiting
  sikc2jgn: number;  // skipped
  azh3qbqs: boolean;  // seen
}

/** The actions `ImportReport` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const IMPORTREPORT_ACTIONS = [] as const;

/** The `tl.so.tilo.contacts.ImportReport` kind at `path`. */
export function importReport(ws: tilo.Workspace, path: string): ImportReport {
  return resourceProxy(ws, path, SURFACE, IMPORTREPORT_ACTIONS) as unknown as ImportReport;
}

/** Book — `tl.so.tilo.contacts.Book`. */
export interface Book extends tilo.Resource {
  qfj7l6eu: Array<tilo.Entry<Contact>>;  // contacts
  uyyz5pg2: Array<tilo.Entry<Group>>;  // groups
  zc4aqhgg: Array<tilo.Entry<ImportCard>>;  // review
  scky6ngy: ImportReport;  // last_import
  /** A new contact, named by what is typed where they were made; a book's entry gives the name it takes, so nothing is left called 'No name yet' by a person who typed one. */
  new_contact(args?: {
    name?: string;
  }): Promise<Contact>;
  /** Finds the contact the workspace keeps under `name`, or adds one: what brings someone in from another service — a task given to them before they joined — to one card, however many imports name them. */
  named_or_new(args: {
    name: string;
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
  /** Deletes a contact from the book, and with it the person the workspace's apps know of it where the window hands the directory: what Mail, Calendar and Messages reach it by goes too. */
  delete(args: {
    c: Contact;
    directory?: unknown;
  }): Promise<void>;
  /** Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteMany(items: DeleteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Merges the card that goes into the one that stays: every number, mailbox, address and group of both is the card that stays, the fields `take` names come from the card that goes, and the card that goes is deleted. One change, so undo brings both cards back. */
  merge_into(args: {
    keep: Contact;
    goes: Contact;
    take?: MergeField[];
    directory?: unknown;
  }): Promise<void>;
  /** Runs `merge_into` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  merge_intoMany(items: MergeIntoArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Every contact as one vCard file, which another address book reads whole. */
  export_book(args?: {
    version?: VCardVersion;
  }): Promise<unknown>;
  /** Every contact as one vCard 3.0 file, which an address book that reads no 4.0 card opens. */
  export_book_v3(): Promise<unknown>;
  /** Every contact as the text of one card file, or the one `name` calls: for an agent or a script that hands the cards on where a person's file chooser cannot, as the address its bytes are kept at. A name the book holds twice is refused rather than guessed. */
  export_vcard_content(args?: {
    name?: string;
  }): Promise<unknown>;
  /** Imports the contacts a vCard file holds into the book, each new card a contact of its own, and answers what it did. A card whose id the book already holds unchanged is nobody new; one that says something else about the person waits in the book's review, and the contact stays as it is until someone says which card is theirs. A file beyond what this app reads is refused whole: what the refusal says is why. */
  import_vcard(args: {
    file: unknown;
  }): Promise<ImportReport>;
  /** The cards `cards`, the words of a vCard file, as contacts of this book, and what became of each: what an importer, an agent or a script hands a book in one call. */
  add_contacts(args: {
    cards: string;
  }): Promise<ImportReport>;
  /** The report of an import of `skipped` cards, and no others: what a file that could not be read answers. */
  refuse_import(args: {
    skipped: number;
  }): Promise<ImportReport>;
  /** What an import did, kept on the book for the window's banner to read and answer. */
  report_import(args: {
    imported: number;
    unchanged: number;
    awaiting: number;
    skipped: number;
  }): Promise<ImportReport>;
  /** The person has read what the last import did: the banner stops showing it. */
  dismiss_import(): Promise<void>;
  /** Runs `dismiss_import` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  dismiss_importMany(items: DismissImportArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Brings one card of a vCard file to the book: takes it as a contact, finds the book already holds it unchanged, puts it up for review where it says something else about a contact the book holds by the same id, or refuses a card that names nobody. `uid` is the id the caller gives a card whose file names none, so the caller can find the contact it makes. */
  file_card(args: {
    card: string;
    uid?: string;
  }): Promise<CardFate>;
  /** The contact a card makes in the book, under the id `id`: what a new card becomes, and what a card someone chose over the one the book held becomes. Its rows are filled after it stands in the book, as every resource's are. */
  take_card(args: {
    card: string;
    id?: string;
  }): Promise<Contact>;
  /** The contact a vCard card says, made in the book, or none for a card that names nobody and for one the book already holds: the single-card form of importing a file. */
  add_card(args: {
    card: string;
  }): Promise<Contact | null>;
  /** Takes a card that waited for review as a contact of its own, beside the one the book holds under the id it named: the file's card is the person's, and the card the book kept keeps everything of its own. Its id is new, because the two cards are two now. */
  import_as_new(args: {
    p: ImportCard;
  }): Promise<Contact>;
  /** Leaves the card the book holds as it is, and takes the file's card out of the review. */
  keep_existing(args: {
    p: ImportCard;
  }): Promise<void>;
  /** Runs `keep_existing` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  keep_existingMany(items: KeepExistingArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The contact the book holds for `address`, as an action: what a person, an agent or another app asks before they write to it. Nothing is changed and nothing is guessed from a name. */
  find_for_address(args: {
    address: string;
  }): Promise<Contact | null>;
}

/** The actions `Book` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const BOOK_ACTIONS = ["new_contact", "named_or_new", "group", "groupMany", "new_group", "delete", "deleteMany", "merge_into", "merge_intoMany", "export_book", "export_book_v3", "export_vcard_content", "import_vcard", "add_contacts", "refuse_import", "report_import", "dismiss_import", "dismiss_importMany", "file_card", "take_card", "add_card", "import_as_new", "keep_existing", "keep_existingMany", "find_for_address"] as const;

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
  /** Shares this card with the workspace's apps, under the person it is: the name, the numbers and the mailboxes another app reads of a contact before it writes to them. The card keeps that person's id, so it stops being shared, or is shared again, as the same person. */
  share(args: {
    d: unknown;
    person: unknown;
    c: Contact;
  }): Promise<void>;
  /** Runs `share` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  shareMany(items: ShareArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shares a card the workspace's apps do not know yet: a person of its own is made for it, so Mail, Calendar and Messages can name it and find it by its numbers and mailboxes. */
  share_card(args: {
    d: unknown;
    c: Contact;
  }): Promise<void>;
  /** Runs `share_card` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  share_cardMany(items: ShareCardArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes the contact out of the directory, leaving its card in the book: what the apps read of it stops naming it, and the card is no person of the directory's until it is shared again. */
  unshare(args: {
    d: unknown;
    person: unknown;
    c: Contact;
  }): Promise<void>;
  /** Runs `unshare` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  unshareMany(items: UnshareArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Contacts` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CONTACTS_ACTIONS = ["confirm", "confirmMany", "change", "changeMany", "forget", "forgetMany", "share", "shareMany", "share_card", "share_cardMany", "unshare", "unshareMany"] as const;

/** The `tl.so.tilo.contacts` kind at `path`. */
export function contacts(ws: tilo.Workspace, path: string): Contacts {
  return resourceProxy(ws, path, SURFACE, CONTACTS_ACTIONS) as unknown as Contacts;
}

