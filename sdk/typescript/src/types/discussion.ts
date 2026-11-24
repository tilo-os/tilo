/**
 * Types for Discussion (`so.tilo.lang.discussion`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Origin` is a `Resource` of the app's `tl.so.tilo.lang.discussion.Origin`, made by its factory with the
 * resource's path (`discussion/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.lang.discussion@d49b0379a6c070b7";

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

/** The arguments of `resolve`. */
export interface ResolveArgs {
  resolved?: boolean;
}

/** The arguments of `delete`. */
export interface DeleteArgs {
  c: Comment;
}

/** Origin — `tl.so.tilo.lang.discussion.Origin`. */
export interface Origin extends tilo.Resource {
  xxyuyumk: string;  // author
  gmvkzzuz: string | null;  // handle
  nfpru2aa: string;  // source
  wuizz4mi: string | null;  // url
  semowww2: string | null;  // reference
  "3wgbb6lz": string | null;  // created
  "2wr4psic": string | null;  // updated
}

/** The actions `Origin` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ORIGIN_ACTIONS = [] as const;

/** The `tl.so.tilo.lang.discussion.Origin` kind at `path`. */
export function discussionOrigin(ws: tilo.Workspace, path: string): Origin {
  return resourceProxy(ws, path, SURFACE, ORIGIN_ACTIONS) as unknown as Origin;
}

/** Comment — `tl.so.tilo.lang.discussion.Comment`. */
export interface Comment extends tilo.Resource {
  "5q4mbecw": string;  // text
  ng7oourf: Comment | null;  // reply_to
  lz4a7tbk: boolean;  // resolved
  c3oavkxa: boolean;  // by_agent
  uiyftp3p: Origin | null;  // origin
  f6337kvt: string;  // step
  hqpdw4ih: string;  // lookup
  tovzcpp5: boolean;  // incorporated
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
  // the app's `edit` action is the SDK's own: run it with `action("edit")`.
  /** Answers a comment: the reply stands under the remark it answers, as a conversation's answer does, and what it is about is what the remark is about. */
  reply(args: {
    text: string;
  }): Promise<Comment>;
  /** Marks a comment as taken care of, or opens it again. Whoever may read the thread may resolve it: resolving says the words were dealt with, not that its author agrees. */
  resolve(args?: {
    resolved?: boolean;
  }): Promise<void>;
  /** Runs `resolve` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  resolveMany(items: ResolveArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Comment` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const COMMENT_ACTIONS = ["editBlock", "editBlockMany", "insertBlock", "insertBlockMany", "deleteBlock", "deleteBlockMany", "reply", "resolve", "resolveMany"] as const;

/** The `tl.so.tilo.lang.discussion.Comment` kind at `path`. */
export function comment(ws: tilo.Workspace, path: string): Comment {
  return resourceProxy(ws, path, SURFACE, COMMENT_ACTIONS) as unknown as Comment;
}

/** Comments — `tl.so.tilo.lang.discussion.Comments`. */
export interface Comments extends tilo.Resource {
  "3g3duquj": Array<tilo.Entry<Comment>>;  // items
  /** Says something about the resource the comment is made from: the thread under a task, a note or a mail says it with `.made_from(r)`, so the comment records what it is about. An agent says something the same way, its comment its own as any person's is. */
  comment(args: {
    text: string;
    origin?: Origin;
    step?: string;
    lookup?: string;
    incorporated?: boolean;
  }): Promise<Comment>;
  /** Deletes a comment. Only its author may. The platform takes it back rather than asking first, and the removal is a version of the comment, so who removed it stays answerable. */
  delete(args: {
    c: Comment;
  }): Promise<void>;
  /** Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteMany(items: DeleteArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Comments` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const COMMENTS_ACTIONS = ["comment", "delete", "deleteMany"] as const;

/** The `tl.so.tilo.lang.discussion.Comments` kind at `path`. */
export function comments(ws: tilo.Workspace, path: string): Comments {
  return resourceProxy(ws, path, SURFACE, COMMENTS_ACTIONS) as unknown as Comments;
}

/** Discussion — `tl.so.tilo.lang.discussion`. */
export interface Discussion extends tilo.Resource {
}

/** The actions `Discussion` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DISCUSSION_ACTIONS = [] as const;

/** The `tl.so.tilo.lang.discussion` kind at `path`. */
export function discussion(ws: tilo.Workspace, path: string): Discussion {
  return resourceProxy(ws, path, SURFACE, DISCUSSION_ACTIONS) as unknown as Discussion;
}

