/**
 * Types for Word Processor (`so.tilo.word-processor`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Document` is a `Resource` of the app's `word-processor.document`, made by its factory with the
 * resource's path (`word-processor/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.word-processor@888e6bf56c260211";

/** One of bool, int, str: an object whose case the app tags it with. */
export type ClipAttr = Record<string, unknown>;

export interface ClipBlock {
  kind: string;
  text: string;
  marks: MarkSpan[];
  level: number | null;
  ordered: boolean | null;
  align: string | null;
  attrs: Record<string, ClipAttr>;
}

export interface Comment {
  anchor: unknown;
  author: string;
  date: string;
  text: string;
  body: ClipBlock[];
  resolved: boolean;
  replies: Reply[];
}

export interface MarkSpan {
  start: number;
  end: number;
  name: string;
  value: MarkValue;
}

/** One of bool, int, str: an object whose case the app tags it with. */
export type MarkValue = Record<string, unknown>;

export interface Reply {
  author: string;
  date: string;
  text: string;
  body: ClipBlock[];
}

export interface ReadDocument {
  file: string;
  format: string;
  text: string | null;
  from: number | null;
  next: number | null;
  total: number | null;
  picture: string | null;
  media_type: string | null;
  bytes: number | null;
  width: number | null;
  height: number | null;
}

/** Document — `word-processor.document`. */
export interface Document extends tilo.Resource {
  body: string;
  comments: Comment[];
  appendParagraph(args: {
    text: string;
  }): Promise<unknown | tilo.Run>;
  replaceBody(args: {
    markdown: string;
  }): Promise<unknown | tilo.Run>;
  replaceText(args: {
    find: string;
    replace: string;
    all?: boolean;
  }): Promise<unknown | tilo.Run>;
  insertTable(args: {
    after: string;
    rows: number;
    columns: number;
  }): Promise<unknown | tilo.Run>;
  addRow(args: {
    table: string;
  }): Promise<unknown | tilo.Run>;
  addColumn(args: {
    table: string;
  }): Promise<unknown | tilo.Run>;
  deleteRow(args: {
    table: string;
    row: number;
  }): Promise<tilo.Run | void>;
  deleteColumn(args: {
    table: string;
    column: number;
  }): Promise<tilo.Run | void>;
  deleteTable(args: {
    table: string;
  }): Promise<tilo.Run | void>;
  deleteImage(args: {
    image: string;
  }): Promise<tilo.Run | void>;
  acceptChange(args: {
    change: string;
  }): Promise<tilo.Run | void>;
  rejectChange(args: {
    change: string;
  }): Promise<tilo.Run | void>;
  acceptAllChanges(): Promise<tilo.Run | void>;
  rejectAllChanges(): Promise<tilo.Run | void>;
  addComment(args: {
    block: string;
    start: number;
    end: number;
    text: string;
  }): Promise<tilo.Run | void>;
  replyToComment(args: {
    comment: string;
    text: string;
    body?: ClipBlock[];
  }): Promise<tilo.Run | void>;
  resolveComment(args: {
    comment: string;
  }): Promise<tilo.Run | void>;
  adoptComments(): Promise<tilo.Run | void>;
  setPageMargins(args: {
    top: number;
    right: number;
    bottom: number;
    left: number;
  }): Promise<tilo.Run | void>;
  generateToc(): Promise<tilo.Run | void>;
  noteFileWritten(args: {
    digest: string;
  }): Promise<tilo.Run | void>;
  shareImage(): Promise<void>;
  readFile(args: {
    file: unknown;
    from?: number;
  }): Promise<ReadDocument>;
  readContent(args: {
    content: string;
    name: string;
    from?: number;
  }): Promise<ReadDocument>;
  exportContent(args?: {
    format?: string;
  }): Promise<void>;
}

/** The `word-processor.document` kind at `path`. */
export function wordProcessorDocument(ws: tilo.Workspace, path: string): Document {
  return resourceProxy(ws, path, SURFACE) as unknown as Document;
}

