/**
 * Types for Notes (`so.tilo.notes`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Notebook` is a `Resource` of the app's `notes.notebook`, made by its factory with the
 * resource's path (`notes/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.notes@4f4053f8b0fc02d4";

export interface MarkdownFile {
  path: string;
  title: string | null;
  markdown: string;
  origin: Origin | null;
}

export interface Origin {
  author: string;
  handle: string | null;
  source: string;
  url: string | null;
  reference: string | null;
  created_ms: number | null;
  updated_ms: number | null;
}

export interface Page {
  title: string;
  parent: string | null;
  body: string;
  tags: string[];
  creator: string | null;
  creator_name: string;
  origin: Origin | null;
  path: string | null;
  reactions: unknown;
}

export interface Tag {
  name: string;
}

/** The arguments of `rename`. */
export interface RenameArgs {
  page: string;
  title: string;
}

/** The arguments of `delete`. */
export interface DeleteArgs {
  page: string;
}

/** The arguments of `move-note`. */
export interface MoveNoteArgs {
  page: string;
  parent?: string;
  before?: string;
}

/** The arguments of `move-up`. */
export interface MoveUpArgs {
  page: string;
}

/** The arguments of `move-down`. */
export interface MoveDownArgs {
  page: string;
}

/** The arguments of `indent`. */
export interface IndentArgs {
  page: string;
}

/** The arguments of `outdent`. */
export interface OutdentArgs {
  page: string;
}

/** The arguments of `duplicate`. */
export interface DuplicateArgs {
  page: string;
}

/** The arguments of `new-tag`. */
export interface NewTagArgs {
  name: string;
}

/** The arguments of `tag`. */
export interface TagArgs {
  page: string;
  name: string;
}

/** The arguments of `delete-tag`. */
export interface DeleteTagArgs {
  tag: string;
}

/** The arguments of `keep-image`. */
export interface KeepImageArgs {
  bytes: unknown;
}

/** The arguments of `place-image`. */
export interface PlaceImageArgs {
  page: string;
  bytes: unknown;
  alt?: string;
}

/** The arguments of `import-markdown`. */
export interface ImportMarkdownArgs {
  title: string;
  markdown: string;
  images: unknown[];
  note?: string;
  path?: string;
  origin?: Origin;
}

/** The arguments of `import-markdown-files`. */
export interface ImportMarkdownFilesArgs {
  files: MarkdownFile[];
}

/** The arguments of `write-note`. */
export interface WriteNoteArgs {
  page: string;
  markdown: string;
}

/** Notebook — `notes.notebook`. */
export interface Notebook extends tilo.Resource {
  pages: Array<tilo.Entry<Page>>;
  tags: Array<tilo.Entry<Tag>>;
  readonly images: Record<string, unknown>;
  newNote(args: {
    title: string;
    parent?: string;
  }): Promise<unknown>;
  newChild(args: {
    page: string;
    title: string;
  }): Promise<unknown>;
  rename(args: {
    page: string;
    title: string;
  }): Promise<void>;
  /** Runs `rename` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  renameMany(items: RenameArgs[]): Promise<tilo.ActionAnswer[]>;
  delete(args: {
    page: string;
  }): Promise<void>;
  /** Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  deleteMany(items: DeleteArgs[]): Promise<tilo.ActionAnswer[]>;
  moveNote(args: {
    page: string;
    parent?: string;
    before?: string;
  }): Promise<void>;
  /** Runs `move-note` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  moveNoteMany(items: MoveNoteArgs[]): Promise<tilo.ActionAnswer[]>;
  moveUp(args: {
    page: string;
  }): Promise<void>;
  /** Runs `move-up` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  moveUpMany(items: MoveUpArgs[]): Promise<tilo.ActionAnswer[]>;
  moveDown(args: {
    page: string;
  }): Promise<void>;
  /** Runs `move-down` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  moveDownMany(items: MoveDownArgs[]): Promise<tilo.ActionAnswer[]>;
  indent(args: {
    page: string;
  }): Promise<void>;
  /** Runs `indent` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  indentMany(items: IndentArgs[]): Promise<tilo.ActionAnswer[]>;
  outdent(args: {
    page: string;
  }): Promise<void>;
  /** Runs `outdent` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  outdentMany(items: OutdentArgs[]): Promise<tilo.ActionAnswer[]>;
  duplicate(args: {
    page: string;
  }): Promise<void>;
  /** Runs `duplicate` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  duplicateMany(items: DuplicateArgs[]): Promise<tilo.ActionAnswer[]>;
  newTag(args: {
    name: string;
  }): Promise<void>;
  /** Runs `new-tag` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  newTagMany(items: NewTagArgs[]): Promise<tilo.ActionAnswer[]>;
  tag(args: {
    page: string;
    name: string;
  }): Promise<void>;
  /** Runs `tag` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  tagMany(items: TagArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteTag(args: {
    tag: string;
  }): Promise<void>;
  /** Runs `delete-tag` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  deleteTagMany(items: DeleteTagArgs[]): Promise<tilo.ActionAnswer[]>;
  keepImage(args: {
    bytes: unknown;
  }): Promise<void>;
  /** Runs `keep-image` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  keepImageMany(items: KeepImageArgs[]): Promise<tilo.ActionAnswer[]>;
  placeImage(args: {
    page: string;
    bytes: unknown;
    alt?: string;
  }): Promise<void>;
  /** Runs `place-image` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  placeImageMany(items: PlaceImageArgs[]): Promise<tilo.ActionAnswer[]>;
  createPage(args: {
    title: string;
    markdown?: string;
    origin?: Origin;
  }): Promise<unknown>;
  importMarkdown(args: {
    title: string;
    markdown: string;
    images: unknown[];
    note?: string;
    path?: string;
    origin?: Origin;
  }): Promise<void>;
  /** Runs `import-markdown` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  importMarkdownMany(items: ImportMarkdownArgs[]): Promise<tilo.ActionAnswer[]>;
  importMarkdownFiles(args: {
    files: MarkdownFile[];
  }): Promise<void>;
  /** Runs `import-markdown-files` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  importMarkdownFilesMany(items: ImportMarkdownFilesArgs[]): Promise<tilo.ActionAnswer[]>;
  appendToNote(args: {
    markdown: string;
    title?: string;
  }): Promise<unknown>;
  writeNote(args: {
    page: string;
    markdown: string;
  }): Promise<void>;
  /** Runs `write-note` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  writeNoteMany(items: WriteNoteArgs[]): Promise<tilo.ActionAnswer[]>;
  exportMarkdownContent(args?: {
    page?: string;
    title?: string;
  }): Promise<void>;
}

/** The actions `Notebook` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const NOTEBOOK_ACTIONS = ["newNote", "newChild", "rename", "renameMany", "delete", "deleteMany", "moveNote", "moveNoteMany", "moveUp", "moveUpMany", "moveDown", "moveDownMany", "indent", "indentMany", "outdent", "outdentMany", "duplicate", "duplicateMany", "newTag", "newTagMany", "tag", "tagMany", "deleteTag", "deleteTagMany", "keepImage", "keepImageMany", "placeImage", "placeImageMany", "createPage", "importMarkdown", "importMarkdownMany", "importMarkdownFiles", "importMarkdownFilesMany", "appendToNote", "writeNote", "writeNoteMany", "exportMarkdownContent"] as const;

/** The `notes.notebook` kind at `path`. */
export function notebook(ws: tilo.Workspace, path: string): Notebook {
  return resourceProxy(ws, path, SURFACE, NOTEBOOK_ACTIONS) as unknown as Notebook;
}

