/**
 * Types for Discussion (`so.tilo.lang.discussion`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Comment` is a `Resource` of the app's `tl.so.tilo.lang.discussion.Comment`, made by its factory with the
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
export const SURFACE = "so.tilo.lang.discussion@3dc800efbd65ec71";

/** The arguments of `delete`. */
export interface DeleteArgs {
  c: Comment;
}

/** Comment — `tl.so.tilo.lang.discussion.Comment`. */
export interface Comment extends tilo.Resource {
  "5q4mbecw": string;  // text
  // the app's `edit` action is the SDK's own: run it with `action("edit")`.
}

/** The actions `Comment` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const COMMENT_ACTIONS = [] as const;

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

