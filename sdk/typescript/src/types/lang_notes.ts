/**
 * Types for Notes (`so.tilo.lang.notes`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Note` is a `Resource` of the app's `tl.so.tilo.lang.notes.Note`, made by its factory with the
 * resource's path (`notes/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.lang.notes@f28cdaaae36373f8";

/** The arguments of `pin`. */
export interface PinArgs {
  pinned?: boolean;
}

/** The arguments of `retitle`. */
export interface RetitleArgs {
  title: string;
}

/** The arguments of `add_text`. */
export interface AddTextArgs {
  text: string;
}

/** The arguments of `tag`. */
export interface TagArgs {
  t: string;
}

/** The arguments of `untag`. */
export interface UntagArgs {
  t: string;
}

/** The arguments of `keep_image`. */
export interface KeepImageArgs {
  picture: string[];
}

/** The arguments of `move_to`. */
export interface MoveToArgs {
  moved: Note;
  parent?: Note;
  before?: Note;
}

/** The arguments of `move_after`. */
export interface MoveAfterArgs {
  moved: Note;
  n: Note;
}

/** The arguments of `sort_children`. */
export interface SortChildrenArgs {
  n: Note;
}

/** The arguments of `delete`. */
export interface DeleteArgs {
  n: Note;
}

/** The arguments of `append_to_note`. */
export interface AppendToNoteArgs {
  title: string;
  text: string;
}

/** Note — `tl.so.tilo.lang.notes.Note`. */
export interface Note extends tilo.Resource {
  dpj7scfe: string;  # title
  olsyp3aw: string;  # body
  pmjerv63: Record<string, string>;  # images
  ssu2tbnf: Note | null;  # parent
  gfk6rqxp: boolean;  # pinned
  xcpasjg7: string[];  # tags
  erijxfd3: string;  # source
  uquoltho: string;  # origin
  ngwafrjq: string;  # by
  buuvovaf: string;  # service
  jznzkj47: string;  # page
  hchfoqqm: string;  # created
  5wdceapz: string;  # updated
  /** Pins or unpins a note. */
  pin(args?: {
    pinned?: boolean;
  }): Promise<void>;
  /** Runs `pin` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  pinMany(items: PinArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Renames a note. */
  retitle(args: {
    title: string;
  }): Promise<void>;
  /** Runs `retitle` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  retitleMany(items: RetitleArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Writes `text` (Markdown) at the end of a note, under what it says already: a line, a list or a paragraph added to it. */
  add_text(args: {
    text: string;
  }): Promise<void>;
  /** Runs `add_text` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  add_textMany(items: AddTextArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Gives a note a tag. */
  tag(args: {
    t: string;
  }): Promise<void>;
  /** Runs `tag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  tagMany(items: TagArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Untags a note. */
  untag(args: {
    t: string;
  }): Promise<void>;
  /** Runs `untag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  untagMany(items: UntagArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps a picture the editor wrote into a note where the person pasted or dropped it: the note holds the address of its bytes, and the name it came in as, which an export writes beside it. `picture` is the editor's word of it: its name, then the address of its content. */
  keep_image(args: {
    picture: string[];
  }): Promise<void>;
  /** Runs `keep_image` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  keep_imageMany(items: KeepImageArgs[]): Promise<tilo.ActionAnswer[]>;
  /** A note as Markdown: its title, then its body, each picture it shows written beside it as `<note>/images/<name>`, a path another reader of the file follows and this app reads again as the address of the picture's bytes. */
  export_markdown(): Promise<string>;
  /** A note as a file: its Markdown, and the pictures it shows beside it. A note that shows a picture is one zip — `Q3 plan.md` and `Q3 plan/images/fox.png` — because Markdown reaches a picture by a path and by nothing else; a note of words alone is that Markdown itself. */
  export_note(): Promise<unknown>;
}

/** The actions `Note` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const NOTE_ACTIONS = ["pin", "pinMany", "retitle", "retitleMany", "add_text", "add_textMany", "tag", "tagMany", "untag", "untagMany", "keep_image", "keep_imageMany", "export_markdown", "export_note"] as const;

/** The `tl.so.tilo.lang.notes.Note` kind at `path`. */
export function note(ws: tilo.Workspace, path: string): Note {
  return resourceProxy(ws, path, SURFACE, NOTE_ACTIONS) as unknown as Note;
}

/** Notebook — `tl.so.tilo.lang.notes.Notebook`. */
export interface Notebook extends tilo.Resource {
  tmqqtkeb: Array<tilo.Entry<Note>>;  # notes
  /** Starts a note at the top of the tree, titled `title` and saying `body` (Markdown) when the caller knows them already. Started from the notebook both are empty: what is typed first names it. */
  new_note(args?: {
    title?: string;
    body?: string;
  }): Promise<Note>;
  /** Starts a note inside `n`'s own, after the notes it already holds. */
  new_child(args: {
    n: Note;
  }): Promise<Note>;
  /** Moves a note to a place in the tree: at the end of `parent`'s own, or where `before` stands. */
  move_to(args: {
    moved: Note;
    parent?: Note;
    before?: Note;
  }): Promise<void>;
  /** Runs `move_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_toMany(items: MoveToArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves a note to just after `n` and every note under it: the place a drop after a row asks for, which names the row it lands after rather than the row after that one. */
  move_after(args: {
    moved: Note;
    n: Note;
  }): Promise<void>;
  /** Runs `move_after` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_afterMany(items: MoveAfterArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Copies a note, with every note under it, just after the one it copies. */
  duplicate(args: {
    n: Note;
  }): Promise<Note>;
  /** Puts a note's own in the order of their titles, once, for a branch that grew out of order. */
  sort_children(args: {
    n: Note;
  }): Promise<void>;
  /** Runs `sort_children` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  sort_childrenMany(items: SortChildrenArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes a note and every note under it, as one change a person takes back at once. */
  delete(args: {
    n: Note;
  }): Promise<void>;
  /** Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteMany(items: DeleteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Writes `text` (Markdown) at the end of the note titled `title`, the case of its letters aside: how an agent or a script that knows a note by its name ('add this to Meeting notes') writes into it without first looking up which note that is. */
  append_to_note(args: {
    title: string;
    text: string;
  }): Promise<void>;
  /** Runs `append_to_note` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  append_to_noteMany(items: AppendToNoteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Files a note titled `title` and tagged `tag` in one step: what another app writes down of what it did (Today's morning) without waiting on the note it made to name it. */
  file_note(args: {
    title: string;
    tag: string;
  }): Promise<Note>;
  /** The notebook as a zip, one Markdown file for each note: a note's own under the folder of the note it stands in, and the pictures it shows in its own `images` folder beside it. That is the shape the import reads, so the notebook comes back as the tree it is, titles, bodies and pictures alike. */
  export_notebook(): Promise<unknown>;
}

/** The actions `Notebook` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const NOTEBOOK_ACTIONS = ["new_note", "new_child", "move_to", "move_toMany", "move_after", "move_afterMany", "duplicate", "sort_children", "sort_childrenMany", "delete", "deleteMany", "append_to_note", "append_to_noteMany", "file_note", "export_notebook"] as const;

/** The `tl.so.tilo.lang.notes.Notebook` kind at `path`. */
export function notebook(ws: tilo.Workspace, path: string): Notebook {
  return resourceProxy(ws, path, SURFACE, NOTEBOOK_ACTIONS) as unknown as Notebook;
}

/** Notes — `tl.so.tilo.lang.notes`. */
export interface Notes extends tilo.Resource {
}

/** The actions `Notes` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const NOTES_ACTIONS = [] as const;

/** The `tl.so.tilo.lang.notes` kind at `path`. */
export function notes(ws: tilo.Workspace, path: string): Notes {
  return resourceProxy(ws, path, SURFACE, NOTES_ACTIONS) as unknown as Notes;
}

