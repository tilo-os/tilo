/**
 * Types for Notes (`so.tilo.notes`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Note` is a `Resource` of the app's `tl.so.tilo.notes.Note`, made by its factory with the
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
export const SURFACE = "so.tilo.notes@0e0640a6e4f771c3";

/** The arguments of `pin`. */
export interface PinArgs {
  pinned?: boolean;
}

/** The arguments of `indent`. */
export interface IndentArgs {
  above?: Note;
}

/** The arguments of `outdent`. */
export interface OutdentArgs {
}

/** The arguments of `move_up`. */
export interface MoveUpArgs {
  above?: Note;
}

/** The arguments of `move_down`. */
export interface MoveDownArgs {
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
  tag: string;
}

/** The arguments of `untag`. */
export interface UntagArgs {
  tag: string;
}

/** The arguments of `react`. */
export interface ReactArgs {
  emoji: string;
}

/** The arguments of `insert_image`. */
export interface InsertImageArgs {
  image: unknown;
  alt?: string;
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

/** The arguments of `rename_tag`. */
export interface RenameTagArgs {
  tag: Tag;
  wanted: string;
}

/** The arguments of `delete_tag`. */
export interface DeleteTagArgs {
  tag: Tag;
}

/** The arguments of `import_archive`. */
export interface ImportArchiveArgs {
  file: unknown;
}

/** The arguments of `import_dropped`. */
export interface ImportDroppedArgs {
  files: unknown[];
}

/** Note — `tl.so.tilo.notes.Note`. */
export interface Note extends tilo.Resource {
  dpj7scfe: string;  // title
  olsyp3aw: string;  // body
  pmjerv63: Record<string, string>;  // images
  ssu2tbnf: Note | null;  // parent
  gfk6rqxp: boolean;  // pinned
  xcpasjg7: string[];  // tags
  ddxl7oum: Record<string, string[]>;  // reactions
  t6igvr56: string;  // reacted
  erijxfd3: string;  // source
  uquoltho: string;  // origin
  eo4enwxh: string;  // by
  yzessjhr: string;  // service
  zex2vawx: string;  // page
  rnhwkv6z: string;  // created
  "2od7mwir": string;  // updated
  /** Starts a note inside `n`'s own, after the notes it already holds, titled `words` when the caller knows them already (a `/page` in one of `n`'s blocks names the page it makes with the words that stood there): the note the window has open too, so a person writing one starts the note it stands under without finding its row. It is `n`'s action, so the window offers it for the note it shows and its row offers it. */
  new_child(args?: {
    words?: string;
  }): Promise<Note>;
  /** Pins or unpins a note. */
  pin(args?: {
    pinned?: boolean;
  }): Promise<void>;
  /** Runs `pin` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  pinMany(items: PinArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts a note under the note it stands over, with the notes under it, as `Tab` does in the tree; with none named, it goes under the note above it. */
  indent(args?: {
    above?: Note;
  }): Promise<void>;
  /** Runs `indent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  indentMany(items: IndentArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts a note beside the note it stands in, just after it and the notes under it, as `Shift-Tab` does in the tree. */
  outdent(): Promise<void>;
  /** Runs `outdent` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  outdentMany(items: OutdentArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts a note just over the note above it, with the notes under it, as `Alt-Up` does in the tree; with none named, over the note just above it. */
  move_up(args?: {
    above?: Note;
  }): Promise<void>;
  /** Runs `move_up` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_upMany(items: MoveUpArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts a note just under the note below it and the notes under that one, with its own notes after it, as `Alt-Down` does in the tree. */
  move_down(): Promise<void>;
  /** Runs `move_down` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_downMany(items: MoveDownArgs[]): Promise<tilo.ActionAnswer[]>;
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
  /** Gives a note the tag `tag`, and lists the tag in the notebook when it lists none of that name yet: a name is how a person, an agent and a script know a tag. */
  tag(args: {
    tag: string;
  }): Promise<void>;
  /** Runs `tag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  tagMany(items: TagArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes the tag `tag` off a note. The notebook keeps listing it, so a person finds it again for another note; deleting it from the notebook takes it off every note. */
  untag(args: {
    tag: string;
  }): Promise<void>;
  /** Runs `untag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  untagMany(items: UntagArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Finds the words `text` in one note, the case of their letters aside, and answers each paragraph holding them with its place in the note and its words: what the window's find bar shows, reachable by an agent or the palette that knows the note but not where in it to look. */
  find_text(args: {
    text: string;
  }): Promise<Found[]>;
  /** Adds your reaction with an emoji to a note, or takes it back when you had reacted with it: a press on one of its pills, or the pick of the one its add button opened. */
  react(args: {
    emoji: string;
  }): Promise<void>;
  /** Runs `react` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  reactMany(items: ReactArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts a picture in a note, where its words end, described by `alt` for a reader who cannot see it: the person picks the file, and the note keeps the address of its content rather than the picture, and the name it came in as. Nothing in `alt` can end the Markdown a picture is written with, so a description holding brackets or parentheses still reads as one picture. */
  insert_image(args: {
    image: unknown;
    alt?: string;
  }): Promise<void>;
  /** Runs `insert_image` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  insert_imageMany(items: InsertImageArgs[]): Promise<tilo.ActionAnswer[]>;
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
export const NOTE_ACTIONS = ["new_child", "pin", "pinMany", "indent", "indentMany", "outdent", "outdentMany", "move_up", "move_upMany", "move_down", "move_downMany", "retitle", "retitleMany", "add_text", "add_textMany", "tag", "tagMany", "untag", "untagMany", "find_text", "react", "reactMany", "insert_image", "insert_imageMany", "keep_image", "keep_imageMany", "export_markdown", "export_note"] as const;

/** The `tl.so.tilo.notes.Note` kind at `path`. */
export function note(ws: tilo.Workspace, path: string): Note {
  return resourceProxy(ws, path, SURFACE, NOTE_ACTIONS) as unknown as Note;
}

/** Notebook — `tl.so.tilo.notes.Notebook`. */
export interface Notebook extends tilo.Resource {
  tmqqtkeb: Array<tilo.Entry<Note>>;  // notes
  qgq6mcy7: Array<tilo.Entry<Note>>;  // set_aside
  dq2mnb22: Array<tilo.Entry<Tag>>;  // tags
  /** Starts a note titled `title` and saying `body` (Markdown) when the caller knows them already. Started from the notebook both are empty: what is typed first names it. It stands under `parent` (at the top of the tree when none) and — where a caller names the note it belongs after (`after`) — at that place among the notes there; without one it comes after the notes already under `parent`, never at the foot of a tree it does not stand in. */
  new_note(args?: {
    title?: string;
    body?: string;
    parent?: Note;
    after?: Note;
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
  /** Makes a tag of the notebook named `name`, or gives the one already named that: the name is what people, agents and scripts know a tag by. */
  new_tag(args?: {
    name?: string;
  }): Promise<Tag>;
  /** Names a tag of the notebook: every note that carries the old name carries the new one. */
  rename_tag(args: {
    tag: Tag;
    wanted: string;
  }): Promise<void>;
  /** Runs `rename_tag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rename_tagMany(items: RenameTagArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes a tag of the notebook: no note carries the name any more, and the notebook stops listing it. */
  delete_tag(args: {
    tag: Tag;
  }): Promise<void>;
  /** Runs `delete_tag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_tagMany(items: DeleteTagArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The notebook as a zip, one Markdown file for each note: a note's own under the folder of the note it stands in, and the pictures it shows in its own `images` folder beside it. That is the shape the import reads, so the notebook comes back as the tree it is, titles, bodies and pictures alike. */
  export_notebook(): Promise<unknown>;
  /** The notebook as one Markdown document: each note a heading one level deeper than the note it stands under (six at the deepest), its body beneath, and the notes under it after it. The whole notebook reads as one text, in the order the tree keeps it. */
  export_notebook_markdown(): Promise<unknown>;
  /** Imports a Markdown file as a note, or opens the note that file already made: a note keeps the address of the file it came from, so importing the same file twice makes one note. The note stands under the note of the folder its file's path names — `Q3 plan/Ideas.md` under `Q3 plan.md` — and at the top of the tree when its path names no folder. Its title is the document's first heading, or the file's name; the heading it says the title with is not said again in its body. */
  import_markdown(args: {
    file: unknown;
  }): Promise<Note>;
  /** Imports a page another service held as a note, keeping whose words it is and where it stood there: what `tilo migrate` calls for each page it brings over, and what a person's own import of a file does without a service's words. A page imported by its address there again opens the note it made, whatever the page says now; the note stands under the note of the folder its file's path names. */
  import_page(args: {
    file: unknown;
    by: string;
    service: string;
    page: string;
    created?: string;
    updated?: string;
  }): Promise<Note>;
  /** Imports a note given as words rather than a file: its title, its body as Markdown, the pictures it shows and the notes its links lead to. A caller holding a page as text — a script, an agent, a migration — makes the note without writing a file first: `pictures` are the files the body names as `![alt](name)`, each by the name it came in as, which the note keeps so an export writes it beside the note again; `links` names, by the path a link leads to, the note it opens. The note stands at the top of the tree. */
  import_text(args: {
    title: string;
    markdown: string;
    pictures?: unknown[];
    links?: Record<string, string>;
  }): Promise<Note>;
  /** Imports a notebook another tool wrote: a zip of pages, or the JSON a Notion API page listing answers. A zip comes in under the notebook, a page under the page that held it, in one change: undo takes the whole import back. That is this app's own notebook written out, a Notion export, and what `tilo migrate` brings from a wiki: a page per `.md` file, its pictures beside it, and a database beside it as a `.csv` that becomes a table at its page. Each page's title is its first heading, or its file's name; the pictures a page shows are the files it stands beside, a reference read from the page's own folder, and a link to a page of the archive is the reference to the note that page made. A migration says where each page came from in its own file beside the pages, and a page that is imported already keeps the note it made. A listing carries no files, so each of its pages is a note at the top of the tree. */
  import_archive(args: {
    file: unknown;
  }): Promise<void>;
  /** Runs `import_archive` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  import_archiveMany(items: ImportArchiveArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Files dropped on the notebook come in one at a time. A Markdown file becomes a note. An archive or a Notion page listing becomes the pages it holds, whether the notebook's own written out or another app's export. One action, so undo takes the whole drop back at once. */
  import_dropped(args: {
    files: unknown[];
  }): Promise<void>;
  /** Runs `import_dropped` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  import_droppedMany(items: ImportDroppedArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Notebook` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const NOTEBOOK_ACTIONS = ["new_note", "move_to", "move_toMany", "move_after", "move_afterMany", "duplicate", "sort_children", "sort_childrenMany", "delete", "deleteMany", "append_to_note", "append_to_noteMany", "file_note", "new_tag", "rename_tag", "rename_tagMany", "delete_tag", "delete_tagMany", "export_notebook", "export_notebook_markdown", "import_markdown", "import_page", "import_text", "import_archive", "import_archiveMany", "import_dropped", "import_droppedMany"] as const;

/** The `tl.so.tilo.notes.Notebook` kind at `path`. */
export function notebook(ws: tilo.Workspace, path: string): Notebook {
  return resourceProxy(ws, path, SURFACE, NOTEBOOK_ACTIONS) as unknown as Notebook;
}

/** Tag — `tl.so.tilo.notes.Tag`. */
export interface Tag extends tilo.Resource {
  itadoh74: string;  // name
}

/** The actions `Tag` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TAG_ACTIONS = [] as const;

/** The `tl.so.tilo.notes.Tag` kind at `path`. */
export function tag(ws: tilo.Workspace, path: string): Tag {
  return resourceProxy(ws, path, SURFACE, TAG_ACTIONS) as unknown as Tag;
}

/** Found — `tl.so.tilo.notes.Found`. */
export interface Found extends tilo.Resource {
  dxxdlopq: number;  // at
  "2vlaqp45": string;  // words
}

/** The actions `Found` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FOUND_ACTIONS = [] as const;

/** The `tl.so.tilo.notes.Found` kind at `path`. */
export function found(ws: tilo.Workspace, path: string): Found {
  return resourceProxy(ws, path, SURFACE, FOUND_ACTIONS) as unknown as Found;
}

/** Notes — `tl.so.tilo.notes`. */
export interface Notes extends tilo.Resource {
}

/** The actions `Notes` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const NOTES_ACTIONS = [] as const;

/** The `tl.so.tilo.notes` kind at `path`. */
export function notes(ws: tilo.Workspace, path: string): Notes {
  return resourceProxy(ws, path, SURFACE, NOTES_ACTIONS) as unknown as Notes;
}

