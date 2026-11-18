/**
 * Types for Browser (`so.tilo.browser`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Application` is a `Resource` of the app's `browser.application`, made by its factory with the
 * resource's path (`browser/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.browser@fa66efeb75407ac8";

export interface Bookmark {
  url: string;
  title: string;
}

export interface RememberedForm {
  signature: string;
  label: string;
  pages: string[];
  questions: RememberedQuestion[];
  submit: string[];
  follows: string;
  grew_from: string;
  seen: number;
}

export interface RememberedQuestion {
  name: string;
  kind: string;
  choices: string[];
  required: boolean;
  fallback: string;
}

export interface Settings {
  adblock_enabled: boolean;
  clear_on_exit: boolean;
  search_engine: string;
  homepage: string;
}

export interface Added {
  url: string;
  title: string;
}

export interface ImportedVisit {
  url: string;
  title: string;
  last: number;
  count: number;
}

export interface Visit {
  title: string;
  last: number;
  count: number;
}

export interface Answer {
  name: string;
  value: string;
}

export interface PageAction {
  seq: number;
  time_ms: number;
  actor: string | null;
  device: string;
  device_name: string;
  device_unknown: boolean;
  how: string;
  action: string;
  ok: boolean;
  code: string;
  message: string;
  url: string;
  to_url: string;
  navigation: string;
  frame: string;
  role: string;
  name: string;
  nth: number;
  secret: boolean;
  text: string | null;
  values: string[] | null;
  answers: Answer[];
}

export interface Tab {
  last: number;
  dropped: number;
  at: number;
  device: string;
}

export interface AddressProfile {
  id: string;
  label: string;
  full_name: string;
  given_name: string;
  family_name: string;
  street: string;
  city: string;
  region: string;
  postal_code: string;
  country: string;
}

/** The arguments of `add-bookmark`. */
export interface AddBookmarkArgs {
  url: string;
  title?: string;
}

/** The arguments of `remove-bookmark`. */
export interface RemoveBookmarkArgs {
  bookmark: string;
}

/** The arguments of `set-site-protection`. */
export interface SetSiteProtectionArgs {
  origin: string;
  enabled: boolean;
}

/** The arguments of `remember-forms`. */
export interface RememberFormsArgs {
  origin: string;
  page: string;
  forms: RememberedForm[];
  whole: boolean;
  at: number;
}

/** The arguments of `forget-form`. */
export interface ForgetFormArgs {
  origin: string;
  signature: string;
}

/** The arguments of `forget-site-forms`. */
export interface ForgetSiteFormsArgs {
  origin: string;
}

/** The arguments of `forget-all-forms`. */
export interface ForgetAllFormsArgs {
}

/** The arguments of `add-bookmark`. */
export interface PersonalBookmarksAddBookmarkArgs {
  url: string;
  title: string;
}

/** The arguments of `remove-bookmark`. */
export interface PersonalBookmarksRemoveBookmarkArgs {
  url: string;
  observed_tags: string[];
}

/** The arguments of `import-bookmarks`. */
export interface ImportBookmarksArgs {
  bundle: string;
}

/** The arguments of `remove-imported-node`. */
export interface RemoveImportedNodeArgs {
  source: string;
  id: string;
}

/** The arguments of `visit`. */
export interface VisitArgs {
  url: string;
  title: string;
  at: number;
}

/** The arguments of `import-history-bundle`. */
export interface ImportHistoryBundleArgs {
  bundle: string;
}

/** The arguments of `name-visit`. */
export interface NameVisitArgs {
  url: string;
  title: string;
}

/** The arguments of `forget`. */
export interface ForgetArgs {
  url: string;
}

/** The arguments of `clear-history`. */
export interface ClearHistoryArgs {
}

/** Browser — `browser.application`. */
export interface Application extends tilo.Resource {
  bookmarks: Array<tilo.Entry<Bookmark>>;
  preferences: Settings;
  readonly site_exceptions: string[];
  readonly site_forms: Record<string, RememberedForm>;
  addBookmark(args: {
    url: string;
    title?: string;
  }): Promise<void>;
  /** Runs `add-bookmark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  addBookmarkMany(items: AddBookmarkArgs[]): Promise<tilo.ActionAnswer[]>;
  removeBookmark(args: {
    bookmark: string;
  }): Promise<void>;
  /** Runs `remove-bookmark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  removeBookmarkMany(items: RemoveBookmarkArgs[]): Promise<tilo.ActionAnswer[]>;
  setSiteProtection(args: {
    origin: string;
    enabled: boolean;
  }): Promise<void>;
  /** Runs `set-site-protection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setSiteProtectionMany(items: SetSiteProtectionArgs[]): Promise<tilo.ActionAnswer[]>;
  rememberForms(args: {
    origin: string;
    page: string;
    forms: RememberedForm[];
    whole: boolean;
    at: number;
  }): Promise<void>;
  /** Runs `remember-forms` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rememberFormsMany(items: RememberFormsArgs[]): Promise<tilo.ActionAnswer[]>;
  forgetForm(args: {
    origin: string;
    signature: string;
  }): Promise<void>;
  /** Runs `forget-form` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  forgetFormMany(items: ForgetFormArgs[]): Promise<tilo.ActionAnswer[]>;
  forgetSiteForms(args: {
    origin: string;
  }): Promise<void>;
  /** Runs `forget-site-forms` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  forgetSiteFormsMany(items: ForgetSiteFormsArgs[]): Promise<tilo.ActionAnswer[]>;
  forgetAllForms(): Promise<void>;
  /** Runs `forget-all-forms` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  forgetAllFormsMany(items: ForgetAllFormsArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Application` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const APPLICATION_ACTIONS = ["addBookmark", "addBookmarkMany", "removeBookmark", "removeBookmarkMany", "setSiteProtection", "setSiteProtectionMany", "rememberForms", "rememberFormsMany", "forgetForm", "forgetFormMany", "forgetSiteForms", "forgetSiteFormsMany", "forgetAllForms", "forgetAllFormsMany"] as const;

/** The `browser.application` kind at `path`. */
export function application(ws: tilo.Workspace, path: string): Application {
  return resourceProxy(ws, path, SURFACE, APPLICATION_ACTIONS) as unknown as Application;
}

/** Personal bookmarks — `browser.personal-bookmarks`. */
export interface PersonalBookmarks extends tilo.Resource {
  readonly added: Record<string, Added>;
  readonly removed: string[];
  readonly receipts: Record<string, string>;
  readonly trees: Record<string, unknown>;
  readonly hidden: string[];
  addBookmark(args: {
    url: string;
    title: string;
  }): Promise<void>;
  /** Runs `add-bookmark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  addBookmarkMany(items: PersonalBookmarksAddBookmarkArgs[]): Promise<tilo.ActionAnswer[]>;
  removeBookmark(args: {
    url: string;
    observed_tags: string[];
  }): Promise<void>;
  /** Runs `remove-bookmark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  removeBookmarkMany(items: PersonalBookmarksRemoveBookmarkArgs[]): Promise<tilo.ActionAnswer[]>;
  importBookmarks(args: {
    bundle: string;
  }): Promise<void>;
  /** Runs `import-bookmarks` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  importBookmarksMany(items: ImportBookmarksArgs[]): Promise<tilo.ActionAnswer[]>;
  removeImportedNode(args: {
    source: string;
    id: string;
  }): Promise<void>;
  /** Runs `remove-imported-node` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  removeImportedNodeMany(items: RemoveImportedNodeArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `PersonalBookmarks` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PERSONALBOOKMARKS_ACTIONS = ["addBookmark", "addBookmarkMany", "removeBookmark", "removeBookmarkMany", "importBookmarks", "importBookmarksMany", "removeImportedNode", "removeImportedNodeMany"] as const;

/** The `browser.personal-bookmarks` kind at `path`. */
export function personalBookmarks(ws: tilo.Workspace, path: string): PersonalBookmarks {
  return resourceProxy(ws, path, SURFACE, PERSONALBOOKMARKS_ACTIONS) as unknown as PersonalBookmarks;
}

/** Browsing history — `browser.browsing-history`. */
export interface BrowsingHistory extends tilo.Resource {
  readonly visits: Record<string, Visit>;
  readonly imported: Record<string, ImportedVisit>;
  readonly import_receipts: Record<string, string>;
  readonly forgotten_imports: string[];
  visit(args: {
    url: string;
    title: string;
    at: number;
  }): Promise<void>;
  /** Runs `visit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  visitMany(items: VisitArgs[]): Promise<tilo.ActionAnswer[]>;
  importHistoryBundle(args: {
    bundle: string;
  }): Promise<void>;
  /** Runs `import-history-bundle` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  importHistoryBundleMany(items: ImportHistoryBundleArgs[]): Promise<tilo.ActionAnswer[]>;
  nameVisit(args: {
    url: string;
    title: string;
  }): Promise<void>;
  /** Runs `name-visit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  nameVisitMany(items: NameVisitArgs[]): Promise<tilo.ActionAnswer[]>;
  forget(args: {
    url: string;
  }): Promise<void>;
  /** Runs `forget` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  forgetMany(items: ForgetArgs[]): Promise<tilo.ActionAnswer[]>;
  clearHistory(): Promise<void>;
  /** Runs `clear-history` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clearHistoryMany(items: ClearHistoryArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `BrowsingHistory` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const BROWSINGHISTORY_ACTIONS = ["visit", "visitMany", "importHistoryBundle", "importHistoryBundleMany", "nameVisit", "nameVisitMany", "forget", "forgetMany", "clearHistory", "clearHistoryMany"] as const;

/** The `browser.browsing-history` kind at `path`. */
export function browsingHistory(ws: tilo.Workspace, path: string): BrowsingHistory {
  return resourceProxy(ws, path, SURFACE, BROWSINGHISTORY_ACTIONS) as unknown as BrowsingHistory;
}

/** Page resource — `browser.page-record`. */
export interface PageRecord extends tilo.Resource {
  readonly actions: Record<string, PageAction>;
  readonly tabs: Record<string, Tab>;
}

/** The actions `PageRecord` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PAGERECORD_ACTIONS = [] as const;

/** The `browser.page-record` kind at `path`. */
export function pageRecord(ws: tilo.Workspace, path: string): PageRecord {
  return resourceProxy(ws, path, SURFACE, PAGERECORD_ACTIONS) as unknown as PageRecord;
}

/** Address book — `browser.address-book`. */
export interface AddressBook extends tilo.Resource {
  enabled: boolean;
  profiles: Array<tilo.Entry<AddressProfile>>;
}

/** The actions `AddressBook` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ADDRESSBOOK_ACTIONS = [] as const;

/** The `browser.address-book` kind at `path`. */
export function addressBook(ws: tilo.Workspace, path: string): AddressBook {
  return resourceProxy(ws, path, SURFACE, ADDRESSBOOK_ACTIONS) as unknown as AddressBook;
}

