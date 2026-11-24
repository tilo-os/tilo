/**
 * Types for Snippets (`so.tilo.snippets`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`snippets/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.snippets@13ad3d7afb13bb2b";

/** The kind of text a snippet is written in: its badge in the list and the colours its body is shown with. One of "text", "shell", "rust", "sql", "git", "python", "javascript", "json", "yaml", "markdown", "html", "css", "toml", "dockerfile". */
export type Language = "text" | "shell" | "rust" | "sql" | "git" | "python" | "javascript" | "json" | "yaml" | "markdown" | "html" | "css" | "toml" | "dockerfile";

/** The arguments of `star`. */
export interface StarArgs {
  starred?: boolean;
}

/** The arguments of `set_tags`. */
export interface SetTagsArgs {
  tags: string;
}

/** The arguments of `untag`. */
export interface UntagArgs {
  tag: string;
}

/** The arguments of `record_use`. */
export interface RecordUseArgs {
}

/** The arguments of `cancel_fill`. */
export interface CancelFillArgs {
}

/** The arguments of `clear_fill`. */
export interface ClearFillArgs {
}

/** The arguments of `delete`. */
export interface DeleteArgs {
  snippet: Snippet;
}

/** The arguments of `rename_folder`. */
export interface RenameFolderArgs {
  folder: string;
  to: string;
}

/** The arguments of `rename_tag`. */
export interface RenameTagArgs {
  tag: string;
  to: string;
}

/** The arguments of `remove_tag`. */
export interface RemoveTagArgs {
  tag: string;
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

/** Snippet — `tl.so.tilo.snippets.Snippet`. */
export interface Snippet extends tilo.Resource {
  f7zljrnp: string;  // title
  "5k3zya5m": string;  // body
  wziqkkiz: string;  // description
  rorcz6y2: boolean;  // favorite
  b5ooyg4x: string;  // folder
  "26cc5hue": Language;  // language
  te2jwa3o: string[];  // tags
  kkfkhxfb: number;  // uses
  /** Stars a snippet, or takes the star back. */
  star(args?: {
    starred?: boolean;
  }): Promise<void>;
  /** Runs `star` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  starMany(items: StarArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Gives a snippet the tags a person typed, comma-separated: each trimmed, lower case, without `#`, at most 24 of them. */
  set_tags(args: {
    tags: string;
  }): Promise<void>;
  /** Runs `set_tags` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_tagsMany(items: SetTagsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes one tag off one snippet. */
  untag(args: {
    tag: string;
  }): Promise<void>;
  /** Runs `untag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  untagMany(items: UntagArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Copies a snippet's body to the clipboard, and counts the use. A snippet with variables is filled in first (`begin_fill`), so what is copied is what the person meant. */
  copy(): Promise<void>;
  /** Counts a use of a snippet without copying it: what a script or an agent that took the body its own way records, so 'Most used' is the same number however the snippet was used. */
  record_use(): Promise<void>;
  /** Runs `record_use` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  record_useMany(items: RecordUseArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Snippet` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SNIPPET_ACTIONS = ["star", "starMany", "set_tags", "set_tagsMany", "untag", "untagMany", "copy", "record_use", "record_useMany"] as const;

/** The `tl.so.tilo.snippets.Snippet` kind at `path`. */
export function snippet(ws: tilo.Workspace, path: string): Snippet {
  return resourceProxy(ws, path, SURFACE, SNIPPET_ACTIONS) as unknown as Snippet;
}

/** FillValue — `tl.so.tilo.snippets.FillValue`. */
export interface FillValue extends tilo.Resource {
  u5ym5ttn: string;  // name
  h5vss6x5: string;  // value
}

/** The actions `FillValue` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FILLVALUE_ACTIONS = [] as const;

/** The `tl.so.tilo.snippets.FillValue` kind at `path`. */
export function fillValue(ws: tilo.Workspace, path: string): FillValue {
  return resourceProxy(ws, path, SURFACE, FILLVALUE_ACTIONS) as unknown as FillValue;
}

/** Fill — `tl.so.tilo.snippets.Fill`. */
export interface Fill extends tilo.Resource {
  qxvejddr: Snippet | null;  // snippet
  e6uc6jtt: Array<tilo.Entry<FillValue>>;  // values
  /** Puts the filling away without copying. */
  cancel_fill(): Promise<void>;
  /** Runs `cancel_fill` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  cancel_fillMany(items: CancelFillArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Empties a filling, so the sheet closes. */
  clear_fill(): Promise<void>;
  /** Runs `clear_fill` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_fillMany(items: ClearFillArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Fill` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FILL_ACTIONS = ["cancel_fill", "cancel_fillMany", "clear_fill", "clear_fillMany"] as const;

/** The `tl.so.tilo.snippets.Fill` kind at `path`. */
export function fill(ws: tilo.Workspace, path: string): Fill {
  return resourceProxy(ws, path, SURFACE, FILL_ACTIONS) as unknown as Fill;
}

/** Library — `tl.so.tilo.snippets.Library`. */
export interface Library extends tilo.Resource {
  g5k73yqh: Array<tilo.Entry<Snippet>>;  // snippets
  avqtjv3w: Array<tilo.Entry<Fill>>;  // fills
  /** Makes a snippet, filed under `folder` when it names one. */
  new_snippet(args: {
    title: string;
    folder?: string;
  }): Promise<Snippet>;
  /** Makes a second copy of a snippet, right after it; its word count starts again from nothing. */
  duplicate(args: {
    snippet: Snippet;
    title?: string;
  }): Promise<Snippet>;
  /** Deletes a snippet from the library. Undo brings it back. */
  delete(args: {
    snippet: Snippet;
  }): Promise<void>;
  /** Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteMany(items: DeleteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Renames a folder everywhere it is used; an empty new name files its snippets under nothing. */
  rename_folder(args: {
    folder: string;
    to: string;
  }): Promise<void>;
  /** Runs `rename_folder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rename_folderMany(items: RenameFolderArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Renames a tag everywhere it is used. */
  rename_tag(args: {
    tag: string;
    to: string;
  }): Promise<void>;
  /** Runs `rename_tag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rename_tagMany(items: RenameTagArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes a tag off every snippet that carries it. */
  remove_tag(args: {
    tag: string;
  }): Promise<void>;
  /** Runs `remove_tag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_tagMany(items: RemoveTagArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Starts filling in a snippet's variables: one field for each variable its body holds, emptied. */
  begin_fill(args: {
    snippet: Snippet;
  }): Promise<Fill>;
  /** Copies the filled body, counts the use, and puts the filling away. */
  copy_filled(args: {
    fill: Fill;
  }): Promise<void>;
}

/** The actions `Library` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LIBRARY_ACTIONS = ["new_snippet", "duplicate", "delete", "deleteMany", "rename_folder", "rename_folderMany", "rename_tag", "rename_tagMany", "remove_tag", "remove_tagMany", "begin_fill", "copy_filled"] as const;

/** The `tl.so.tilo.snippets.Library` kind at `path`. */
export function library(ws: tilo.Workspace, path: string): Library {
  return resourceProxy(ws, path, SURFACE, LIBRARY_ACTIONS) as unknown as Library;
}

/** Snippets — `tl.so.tilo.snippets`. */
export interface Snippets extends tilo.Resource {
  /** Puts `text` on the clipboard: as a secret, which histories do not keep and which leaves the clipboard after a while, when it looks like it carries a credential, so an API key does not stay there for whatever is pasted next. False when the device refused the copy. */
  copy_text(args: {
    text: string;
  }): Promise<boolean>;
}

/** The actions `Snippets` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SNIPPETS_ACTIONS = ["copy_text"] as const;

/** The `tl.so.tilo.snippets` kind at `path`. */
export function snippets(ws: tilo.Workspace, path: string): Snippets {
  return resourceProxy(ws, path, SURFACE, SNIPPETS_ACTIONS) as unknown as Snippets;
}

