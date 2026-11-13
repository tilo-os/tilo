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
export const SURFACE = "so.tilo.browser@b99f0cb69ffd791f";

export interface Bookmark {
  url: string;
  title: string;
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

/** Browser — `browser.application`. */
export interface Application extends tilo.Resource {
  bookmarks: Bookmark[];
  preferences: Settings;
  readonly site_exceptions: string[];
  addBookmark(args: {
    url: string;
    title?: string;
  }): Promise<tilo.Run | void>;
  removeBookmark(args: {
    bookmark: string;
  }): Promise<tilo.Run | void>;
  setSiteProtection(args: {
    origin: string;
    enabled: boolean;
  }): Promise<tilo.Run | void>;
}

/** The `browser.application` kind at `path`. */
export function application(ws: tilo.Workspace, path: string): Application {
  return resourceProxy(ws, path, SURFACE) as unknown as Application;
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
  }): Promise<tilo.Run | void>;
  removeBookmark(args: {
    url: string;
    observed_tags: string[];
  }): Promise<tilo.Run | void>;
  importBookmarks(args: {
    bundle: string;
  }): Promise<tilo.Run | void>;
  removeImportedNode(args: {
    source: string;
    id: string;
  }): Promise<tilo.Run | void>;
}

/** The `browser.personal-bookmarks` kind at `path`. */
export function personalBookmarks(ws: tilo.Workspace, path: string): PersonalBookmarks {
  return resourceProxy(ws, path, SURFACE) as unknown as PersonalBookmarks;
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
  }): Promise<tilo.Run | void>;
  importHistoryBundle(args: {
    bundle: string;
  }): Promise<tilo.Run | void>;
  nameVisit(args: {
    url: string;
    title: string;
  }): Promise<tilo.Run | void>;
  forget(args: {
    url: string;
  }): Promise<tilo.Run | void>;
  clearHistory(): Promise<tilo.Run | void>;
}

/** The `browser.browsing-history` kind at `path`. */
export function browsingHistory(ws: tilo.Workspace, path: string): BrowsingHistory {
  return resourceProxy(ws, path, SURFACE) as unknown as BrowsingHistory;
}

/** Address book — `browser.address-book`. */
export interface AddressBook extends tilo.Resource {
  enabled: boolean;
  profiles: AddressProfile[];
}

/** The `browser.address-book` kind at `path`. */
export function addressBook(ws: tilo.Workspace, path: string): AddressBook {
  return resourceProxy(ws, path, SURFACE) as unknown as AddressBook;
}

