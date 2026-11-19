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
export const SURFACE = "so.tilo.feeds@76e15e7c0d655457";

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

/** One of "Heading1", "Heading2", "Heading3", "Paragraph", "Bullet", "Quote", "Code", "Link", "Divider". */
export type BlockKind = "Heading1" | "Heading2" | "Heading3" | "Paragraph" | "Bullet" | "Quote" | "Code" | "Link" | "Divider";

export interface ContentBlock {
  kind: BlockKind;
  text: string;
  href: string | null;
}

export interface Fetched {
  title: string;
  author: string;
  url: string;
  published: number;
  summary: string;
  blocks: ContentBlock[];
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

/** The arguments of `note-feed`. */
export interface NoteFeedArgs {
  feed: string;
  title?: string;
  certificate_pin?: string;
}

/** The arguments of `receive`. */
export interface ReceiveArgs {
  feed: string;
  articles: Fetched[];
}

/** The arguments of `mark-read`. */
export interface MarkReadArgs {
  feed?: string;
}

/** The arguments of `set-read`. */
export interface SetReadArgs {
  article: string;
  read: boolean;
}

/** The arguments of `set-starred`. */
export interface SetStarredArgs {
  article: string;
  starred: boolean;
}

/** The arguments of `set-tags`. */
export interface SetTagsArgs {
  article: string;
  tags: string[];
}

/** The arguments of `forget-feed`. */
export interface ForgetFeedArgs {
  feed: string;
}

/** Feeds — `feeds.library`. */
export interface Library extends tilo.Resource {
  folders: Array<tilo.Entry<Folder>>;
  feeds: Array<tilo.Entry<Feed>>;
  addSubscription(args: {
    url: string;
    folder?: string;
  }): Promise<void>;
  /** Runs `add-subscription` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  addSubscriptionMany(items: AddSubscriptionArgs[]): Promise<tilo.ActionAnswer[]>;
  unsubscribe(args: {
    feed: string;
  }): Promise<void>;
  /** Runs `unsubscribe` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  unsubscribeMany(items: UnsubscribeArgs[]): Promise<tilo.ActionAnswer[]>;
  newFolder(args: {
    name: string;
  }): Promise<void>;
  /** Runs `new-folder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  newFolderMany(items: NewFolderArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteFolder(args: {
    folder: string;
  }): Promise<void>;
  /** Runs `delete-folder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteFolderMany(items: DeleteFolderArgs[]): Promise<tilo.ActionAnswer[]>;
  importSubscriptions(args: {
    outlines: Outline[];
  }): Promise<void>;
  /** Runs `import-subscriptions` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  importSubscriptionsMany(items: ImportSubscriptionsArgs[]): Promise<tilo.ActionAnswer[]>;
  noteFeed(args: {
    feed: string;
    title?: string;
    certificate_pin?: string;
  }): Promise<void>;
  /** Runs `note-feed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  noteFeedMany(items: NoteFeedArgs[]): Promise<tilo.ActionAnswer[]>;
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

/** The actions `Library` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LIBRARY_ACTIONS = ["addSubscription", "addSubscriptionMany", "unsubscribe", "unsubscribeMany", "newFolder", "newFolderMany", "deleteFolder", "deleteFolderMany", "importSubscriptions", "importSubscriptionsMany", "noteFeed", "noteFeedMany", "exportOpml", "readerTextLarger", "readerTextSmaller", "readerFont", "readerPaper"] as const;

/** The `feeds.library` kind at `path`. */
export function library(ws: tilo.Workspace, path: string): Library {
  return resourceProxy(ws, path, SURFACE, LIBRARY_ACTIONS) as unknown as Library;
}

/** Articles — `feeds.articles`. */
export interface Articles extends tilo.Resource {
  articles: Array<tilo.Entry<Article>>;
  readonly read_through: Record<string, number>;
  receive(args: {
    feed: string;
    articles: Fetched[];
  }): Promise<void>;
  /** Runs `receive` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  receiveMany(items: ReceiveArgs[]): Promise<tilo.ActionAnswer[]>;
  markRead(args?: {
    feed?: string;
  }): Promise<void>;
  /** Runs `mark-read` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  markReadMany(items: MarkReadArgs[]): Promise<tilo.ActionAnswer[]>;
  setRead(args: {
    article: string;
    read: boolean;
  }): Promise<void>;
  /** Runs `set-read` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setReadMany(items: SetReadArgs[]): Promise<tilo.ActionAnswer[]>;
  setStarred(args: {
    article: string;
    starred: boolean;
  }): Promise<void>;
  /** Runs `set-starred` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setStarredMany(items: SetStarredArgs[]): Promise<tilo.ActionAnswer[]>;
  setTags(args: {
    article: string;
    tags: string[];
  }): Promise<void>;
  /** Runs `set-tags` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setTagsMany(items: SetTagsArgs[]): Promise<tilo.ActionAnswer[]>;
  forgetFeed(args: {
    feed: string;
  }): Promise<void>;
  /** Runs `forget-feed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  forgetFeedMany(items: ForgetFeedArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Articles` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ARTICLES_ACTIONS = ["receive", "receiveMany", "markRead", "markReadMany", "setRead", "setReadMany", "setStarred", "setStarredMany", "setTags", "setTagsMany", "forgetFeed", "forgetFeedMany"] as const;

/** The `feeds.articles` kind at `path`. */
export function articles(ws: tilo.Workspace, path: string): Articles {
  return resourceProxy(ws, path, SURFACE, ARTICLES_ACTIONS) as unknown as Articles;
}

