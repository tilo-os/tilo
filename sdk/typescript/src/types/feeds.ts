/**
 * Types for Feeds (`so.tilo.feeds`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Library` is a `Resource` of the app's `feeds.library`, made by its factory with the
 * resource's path (`feeds/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.feeds@a405395fdcfa75c6";

export interface Article {
  feed_id: string;
  title: string;
  author: string;
  url: string;
  published: number;
  summary: string;
  blocks: ContentBlock[];
  read: boolean;
  starred: boolean;
  tags: string[];
}

/** One of "heading1", "heading2", "heading3", "paragraph", "bullet", "quote", "code", "link", "divider". */
export type BlockKind = "heading1" | "heading2" | "heading3" | "paragraph" | "bullet" | "quote" | "code" | "link" | "divider";

export interface ContentBlock {
  kind: BlockKind;
  text: string;
  href: string | null;
}

export interface Feed {
  title: string;
  url: string;
  site_url: string;
  folder: string | null;
  kind: FeedKind;
  certificate_pin: string | null;
}

/** One of "rss", "atom", "json", "gemini". */
export type FeedKind = "rss" | "atom" | "json" | "gemini";

export interface Fetched {
  title: string;
  author: string;
  url: string;
  published: number;
  summary: string;
  blocks: ContentBlock[];
}

export interface Folder {
  name: string;
  collapsed: boolean;
}

export interface Outline {
  title: string;
  url: string;
  site_url: string;
  folder: string | null;
}

/** The arguments of `add-subscription`. */
export interface AddSubscriptionArgs {
  url: string;
  folder?: string;
}

/** The arguments of `unsubscribe`. */
export interface UnsubscribeArgs {
  feed: string;
}

/** The arguments of `mark-feed-read`. */
export interface MarkFeedReadArgs {
  feed: string;
}

/** The arguments of `mark-everything-read`. */
export interface MarkEverythingReadArgs {
}

/** The arguments of `new-folder`. */
export interface NewFolderArgs {
  name: string;
}

/** The arguments of `delete-folder`. */
export interface DeleteFolderArgs {
  folder: string;
}

/** The arguments of `import-subscriptions`. */
export interface ImportSubscriptionsArgs {
  outlines: Outline[];
}

/** The arguments of `refreshed`. */
export interface RefreshedArgs {
  articles: Fetched[];
  feed: string;
  title?: string;
  certificate_pin?: string;
}

/** Feeds — `feeds.library`. */
export interface Library extends tilo.Resource {
  folders: Folder[];
  feeds: Feed[];
  articles: Article[];
  addSubscription(args: {
    url: string;
    folder?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `add-subscription` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  addSubscriptionMany(items: AddSubscriptionArgs[]): Promise<tilo.ActionAnswer[]>;
  unsubscribe(args: {
    feed: string;
  }): Promise<tilo.Run | void>;
  /** Runs `unsubscribe` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  unsubscribeMany(items: UnsubscribeArgs[]): Promise<tilo.ActionAnswer[]>;
  markFeedRead(args: {
    feed: string;
  }): Promise<tilo.Run | void>;
  /** Runs `mark-feed-read` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  markFeedReadMany(items: MarkFeedReadArgs[]): Promise<tilo.ActionAnswer[]>;
  markEverythingRead(): Promise<tilo.Run | void>;
  /** Runs `mark-everything-read` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  markEverythingReadMany(items: MarkEverythingReadArgs[]): Promise<tilo.ActionAnswer[]>;
  newFolder(args: {
    name: string;
  }): Promise<tilo.Run | void>;
  /** Runs `new-folder` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  newFolderMany(items: NewFolderArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteFolder(args: {
    folder: string;
  }): Promise<tilo.Run | void>;
  /** Runs `delete-folder` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  deleteFolderMany(items: DeleteFolderArgs[]): Promise<tilo.ActionAnswer[]>;
  importSubscriptions(args: {
    outlines: Outline[];
  }): Promise<tilo.Run | void>;
  /** Runs `import-subscriptions` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  importSubscriptionsMany(items: ImportSubscriptionsArgs[]): Promise<tilo.ActionAnswer[]>;
  refreshed(args: {
    articles: Fetched[];
    feed: string;
    title?: string;
    certificate_pin?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `refreshed` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  refreshedMany(items: RefreshedArgs[]): Promise<tilo.ActionAnswer[]>;
  exportOpml(): Promise<void>;
  readerTextLarger(): Promise<void>;
  readerTextSmaller(): Promise<void>;
  readerFont(args: {
    font: string;
  }): Promise<void>;
  readerPaper(args: {
    paper: string;
  }): Promise<void>;
}

/** The `feeds.library` kind at `path`. */
export function library(ws: tilo.Workspace, path: string): Library {
  return resourceProxy(ws, path, SURFACE) as unknown as Library;
}

