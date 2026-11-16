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
 * says where it is. An undoable action that answers nothing also has a `<action>Many`,
which applies it to every argument object of a list in a few all-or-nothing steps,
all of them one run.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** The version of the surface these types were made from: a workspace whose app
 * publishes another refuses their actions rather than run what they no longer
 * describe. */
export const SURFACE = "so.tilo.word-processor@d6a33f5b43133b78";

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

/** The arguments of `delete-row`. */
export interface DeleteRowArgs {
  table: string;
  row: number;
}

/** The arguments of `delete-column`. */
export interface DeleteColumnArgs {
  table: string;
  column: number;
}

/** The arguments of `delete-table`. */
export interface DeleteTableArgs {
  table: string;
}

/** The arguments of `delete-image`. */
export interface DeleteImageArgs {
  image: string;
}

/** The arguments of `accept-change`. */
export interface AcceptChangeArgs {
  change: string;
}

/** The arguments of `reject-change`. */
export interface RejectChangeArgs {
  change: string;
}

/** The arguments of `accept-all-changes`. */
export interface AcceptAllChangesArgs {
}

/** The arguments of `reject-all-changes`. */
export interface RejectAllChangesArgs {
}

/** The arguments of `add-comment`. */
export interface AddCommentArgs {
  block: string;
  start: number;
  end: number;
  text: string;
}

/** The arguments of `reply-to-comment`. */
export interface ReplyToCommentArgs {
  comment: string;
  text: string;
  body?: ClipBlock[];
}

/** The arguments of `resolve-comment`. */
export interface ResolveCommentArgs {
  comment: string;
}

/** The arguments of `adopt-comments`. */
export interface AdoptCommentsArgs {
}

/** The arguments of `set-page-margins`. */
export interface SetPageMarginsArgs {
  top: number;
  right: number;
  bottom: number;
  left: number;
}

/** The arguments of `generate-toc`. */
export interface GenerateTocArgs {
}

/** The arguments of `note-file-written`. */
export interface NoteFileWrittenArgs {
  digest: string;
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
  /** Runs `delete-row` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  deleteRowMany(items: DeleteRowArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteColumn(args: {
    table: string;
    column: number;
  }): Promise<tilo.Run | void>;
  /** Runs `delete-column` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  deleteColumnMany(items: DeleteColumnArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteTable(args: {
    table: string;
  }): Promise<tilo.Run | void>;
  /** Runs `delete-table` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  deleteTableMany(items: DeleteTableArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteImage(args: {
    image: string;
  }): Promise<tilo.Run | void>;
  /** Runs `delete-image` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  deleteImageMany(items: DeleteImageArgs[]): Promise<tilo.ActionAnswer[]>;
  acceptChange(args: {
    change: string;
  }): Promise<tilo.Run | void>;
  /** Runs `accept-change` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  acceptChangeMany(items: AcceptChangeArgs[]): Promise<tilo.ActionAnswer[]>;
  rejectChange(args: {
    change: string;
  }): Promise<tilo.Run | void>;
  /** Runs `reject-change` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  rejectChangeMany(items: RejectChangeArgs[]): Promise<tilo.ActionAnswer[]>;
  acceptAllChanges(): Promise<tilo.Run | void>;
  /** Runs `accept-all-changes` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  acceptAllChangesMany(items: AcceptAllChangesArgs[]): Promise<tilo.ActionAnswer[]>;
  rejectAllChanges(): Promise<tilo.Run | void>;
  /** Runs `reject-all-changes` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  rejectAllChangesMany(items: RejectAllChangesArgs[]): Promise<tilo.ActionAnswer[]>;
  addComment(args: {
    block: string;
    start: number;
    end: number;
    text: string;
  }): Promise<tilo.Run | void>;
  /** Runs `add-comment` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  addCommentMany(items: AddCommentArgs[]): Promise<tilo.ActionAnswer[]>;
  replyToComment(args: {
    comment: string;
    text: string;
    body?: ClipBlock[];
  }): Promise<tilo.Run | void>;
  /** Runs `reply-to-comment` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  replyToCommentMany(items: ReplyToCommentArgs[]): Promise<tilo.ActionAnswer[]>;
  resolveComment(args: {
    comment: string;
  }): Promise<tilo.Run | void>;
  /** Runs `resolve-comment` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  resolveCommentMany(items: ResolveCommentArgs[]): Promise<tilo.ActionAnswer[]>;
  adoptComments(): Promise<tilo.Run | void>;
  /** Runs `adopt-comments` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  adoptCommentsMany(items: AdoptCommentsArgs[]): Promise<tilo.ActionAnswer[]>;
  setPageMargins(args: {
    top: number;
    right: number;
    bottom: number;
    left: number;
  }): Promise<tilo.Run | void>;
  /** Runs `set-page-margins` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  setPageMarginsMany(items: SetPageMarginsArgs[]): Promise<tilo.ActionAnswer[]>;
  generateToc(): Promise<tilo.Run | void>;
  /** Runs `generate-toc` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  generateTocMany(items: GenerateTocArgs[]): Promise<tilo.ActionAnswer[]>;
  noteFileWritten(args: {
    digest: string;
  }): Promise<tilo.Run | void>;
  /** Runs `note-file-written` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  noteFileWrittenMany(items: NoteFileWrittenArgs[]): Promise<tilo.ActionAnswer[]>;
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

