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
 * says where it is.
 */

import { resourceProxy } from "@tilo/sdk";
import type * as tilo from "@tilo/sdk";

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

/** Feeds — `feeds.library`. */
export interface Library extends tilo.Resource {
  folders: Folder[];
  feeds: Feed[];
  articles: Article[];
  addSubscription(args: {
    url: string;
    folder?: string;
  }): Promise<tilo.Run | void>;
  unsubscribe(args: {
    feed: string;
  }): Promise<tilo.Run | void>;
  markFeedRead(args: {
    feed: string;
  }): Promise<tilo.Run | void>;
  markEverythingRead(): Promise<tilo.Run | void>;
  newFolder(args: {
    name: string;
  }): Promise<tilo.Run | void>;
  deleteFolder(args: {
    folder: string;
  }): Promise<tilo.Run | void>;
  importSubscriptions(args: {
    outlines: Outline[];
  }): Promise<tilo.Run | void>;
  refreshed(args: {
    articles: Fetched[];
    feed: string;
    title?: string;
    certificate_pin?: string;
  }): Promise<tilo.Run | void>;
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
  return resourceProxy(ws, path) as unknown as Library;
}

