/**
 * Types for PDF Viewer (`so.tilo.pdf-viewer`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Document` is a `Resource` of the app's `tl.so.tilo.pdf-viewer.Document`, made by its factory with the
 * resource's path (`pdf-viewer/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.pdf-viewer@69f643c65a28bb23";

/** What a mark does on a page. One of "markup", "ink", "note", "text". */
export type MarkKind = "markup" | "ink" | "note" | "text";

/** The arguments of `lay_out`. */
export interface LayOutArgs {
}

/** The arguments of `read_outline`. */
export interface ReadOutlineArgs {
}

/** The arguments of `read_fields`. */
export interface ReadFieldsArgs {
}

/** The arguments of `read_page`. */
export interface ReadPageArgs {
  p: Page;
}

/** The arguments of `read_links`. */
export interface ReadLinksArgs {
  p: Page;
}

/** The arguments of `show_page`. */
export interface ShowPageArgs {
  p: Page;
  width: number;
}

/** The arguments of `read_next`. */
export interface ReadNextArgs {
}

/** The arguments of `draw_next`. */
export interface DrawNextArgs {
}

/** The arguments of `rotate_page`. */
export interface RotatePageArgs {
  p: Page;
}

/** The arguments of `move_page`. */
export interface MovePageArgs {
  p: Page;
  after?: Page;
}

/** The arguments of `delete_page`. */
export interface DeletePageArgs {
  p: Page;
}

/** The arguments of `import_pages`. */
export interface ImportPagesArgs {
  file: unknown;
  after?: Page;
}

/** The arguments of `add_note`. */
export interface AddNoteArgs {
  p: Page;
  words: string;
}

/** The arguments of `write_mark`. */
export interface WriteMarkArgs {
  m: Mark;
  words: string;
  rects?: number[];
}

/** The arguments of `move_mark`. */
export interface MoveMarkArgs {
  m: Mark;
  x: number;
  y: number;
}

/** The arguments of `recolour`. */
export interface RecolourArgs {
  m: Mark;
  colour: string;
}

/** The arguments of `delete_mark`. */
export interface DeleteMarkArgs {
  m: Mark;
}

/** The arguments of `resolve_comment`. */
export interface ResolveCommentArgs {
  c: Comment;
  resolved: boolean;
}

/** The arguments of `edit_comment`. */
export interface EditCommentArgs {
  c: Comment;
  words: string;
}

/** The arguments of `delete_comment`. */
export interface DeleteCommentArgs {
  c: Comment;
}

/** The arguments of `delete_reply`. */
export interface DeleteReplyArgs {
  c: Comment;
  r: Reply;
}

/** The arguments of `sign_with_name`. */
export interface SignWithNameArgs {
  p: Page;
  signer: string;
}

/** The arguments of `delete_signature`. */
export interface DeleteSignatureArgs {
  s: Signature;
}

/** The arguments of `fill_field`. */
export interface FillFieldArgs {
  name: string;
  text: string;
}

/** The arguments of `choose_field`. */
export interface ChooseFieldArgs {
  name: string;
  choice: string;
}

/** The arguments of `check_field`. */
export interface CheckFieldArgs {
  name: string;
  on: boolean;
  export?: string;
}

/** The arguments of `remember_reading`. */
export interface RememberReadingArgs {
  p: Page;
  fraction: number;
  zoom: number;
}

/** The arguments of `adopt_markup`. */
export interface AdoptMarkupArgs {
}

/** The arguments of `turn_to`. */
export interface TurnToArgs {
  at: number;
  zoom: number;
}

/** The arguments of `reorder`. */
export interface ReorderArgs {
  index: number;
  at: number;
}

/** The arguments of `press`. */
export interface PressArgs {
  p: Page;
  at: number[];
  tool: string;
  colour: string;
}

/** The arguments of `release`. */
export interface ReleaseArgs {
  p: Page;
  tool: string;
  colour: string;
  stroke: number[];
  start?: number[];
  end?: number[];
}

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

/** The arguments of `edit-block`. */
export interface ReplyEditBlockArgs {
  block: string;
  text: string;
}

/** The arguments of `insert-block`. */
export interface ReplyInsertBlockArgs {
  block: string;
  text: string;
}

/** The arguments of `delete-block`. */
export interface ReplyDeleteBlockArgs {
  block: string;
}

/** The arguments of `sign_as`. */
export interface SignAsArgs {
  name: string;
}

/** The arguments of `draw_signature`. */
export interface DrawSignatureArgs {
  strokes: number[][];
}

/** Document — `tl.so.tilo.pdf-viewer.Document`. */
export interface Document extends tilo.Resource {
  peajfzyh: string;  // source
  mxgrycmo: string;  // source_name
  ebwmtj4p: string;  // trouble
  "2n6kpf64": string;  // imported_source
  zst2moht: string;  // save_trouble
  vhujxgcb: Array<tilo.Entry<Page>>;  // pages
  zhtkbfeh: Array<tilo.Entry<Picture>>;  // pictures
  "22zxqkpo": Array<tilo.Entry<Outline>>;  // outlines
  "7gcxsbu5": Array<tilo.Entry<Link>>;  // links
  oushu5xx: Array<tilo.Entry<Field>>;  // fields
  ilzd6zzn: Array<tilo.Entry<Mark>>;  // marks
  "7zfoarqk": Array<tilo.Entry<Comment>>;  // comments
  s7stggro: Array<tilo.Entry<Signature>>;  // signatures
  uwnyzedo: Record<string, string>;  // form_text
  zoztpthf: Record<string, string>;  // form_choice
  "2waifcc4": Record<string, boolean>;  // form_checked
  kew5sddy: Record<string, string>;  // form_export
  cvhgaqat: Record<string, string>;  // adopted
  hflhpsk4: Page | null;  // reading_page
  au5nrsiz: number;  // reading_fraction
  fg6xtkn7: number;  // reading_zoom
  /** Lays a document out on its own bytes: one page for each page of the file, in their order, at the size the file draws it, its contents and its form. */
  lay_out(): Promise<void>;
  /** Runs `lay_out` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  lay_outMany(items: LayOutArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reads the document's contents, so the review panel lists them without asking the engine each time it is shown. */
  read_outline(): Promise<void>;
  /** Runs `read_outline` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  read_outlineMany(items: ReadOutlineArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reads the document's form fields, so their widgets are drawn over the pages and filled in place. */
  read_fields(): Promise<void>;
  /** Runs `read_fields` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  read_fieldsMany(items: ReadFieldsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reads the page's words once, so the workspace's search finds the page by them and a person can select them, and reads the links it carries. */
  read_page(args: {
    p: Page;
  }): Promise<void>;
  /** Runs `read_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  read_pageMany(items: ReadPageArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reads the links the page carries: each area it covers and where it leads. */
  read_links(args: {
    p: Page;
  }): Promise<void>;
  /** Runs `read_links` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  read_linksMany(items: ReadLinksArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Draws one page as a picture at `width` pixels, kept so a person looking at it again, or another member of the workspace, does not wait for the engine. */
  show_page(args: {
    p: Page;
    width: number;
  }): Promise<void>;
  /** Runs `show_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  show_pageMany(items: ShowPageArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reads the words of the next page nobody read. */
  read_next(): Promise<void>;
  /** Runs `read_next` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  read_nextMany(items: ReadNextArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Draws the next page nobody drew at the reader's zoom. */
  draw_next(): Promise<void>;
  /** Runs `draw_next` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  draw_nextMany(items: DrawNextArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Turns a page a quarter clockwise. */
  rotate_page(args: {
    p: Page;
  }): Promise<void>;
  /** Runs `rotate_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rotate_pageMany(items: RotatePageArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts a page after another one, or first. */
  move_page(args: {
    p: Page;
    after?: Page;
  }): Promise<void>;
  /** Runs `move_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_pageMany(items: MovePageArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Copies a page right after itself. The marks stay with the page they were made on. */
  duplicate_page(args: {
    p: Page;
  }): Promise<Page>;
  /** Takes a page out of the document; a document keeps at least one. */
  delete_page(args: {
    p: Page;
  }): Promise<void>;
  /** Runs `delete_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_pageMany(items: DeletePageArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes the pages of another PDF or of a picture into the document, after a page (first when none is named). */
  import_pages(args: {
    file: unknown;
    after?: Page;
  }): Promise<void>;
  /** Runs `import_pages` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  import_pagesMany(items: ImportPagesArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Marks text on a page: a highlight, an underline, a strikethrough or a squiggle over the lines a selection covers, in the reader's colour. */
  mark_text(args: {
    p: Page;
    style: string;
    colour: string;
    rects: number[];
  }): Promise<Mark>;
  /** Draws ink on a page: one list of points a stroke, its points as x and y in turn in fractions of the page's box. */
  draw_ink(args: {
    p: Page;
    colour: string;
    strokes: number[][];
  }): Promise<Mark>;
  /** Puts a sticky note on a page where the person pressed, with nothing written in it yet: what it says is written in the conversation it starts. */
  new_note(args: {
    p: Page;
    x: number;
    y: number;
    colour: string;
  }): Promise<Mark>;
  /** Adds a note with these words at the top of a page: for the palette, a script and an agent. */
  add_note(args: {
    p: Page;
    words: string;
  }): Promise<void>;
  /** Runs `add_note` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  add_noteMany(items: AddNoteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts a box of words on a page. */
  new_text_box(args: {
    p: Page;
    x: number;
    y: number;
    width: number;
    height: number;
  }): Promise<Mark>;
  /** Changes what a mark says, and its box when one is named. */
  write_mark(args: {
    m: Mark;
    words: string;
    rects?: number[];
  }): Promise<void>;
  /** Runs `write_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  write_markMany(items: WriteMarkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts a note where the person moved it. */
  move_mark(args: {
    m: Mark;
    x: number;
    y: number;
  }): Promise<void>;
  /** Runs `move_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_markMany(items: MoveMarkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Gives a mark another colour. */
  recolour(args: {
    m: Mark;
    colour: string;
  }): Promise<void>;
  /** Runs `recolour` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  recolourMany(items: RecolourArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes a mark away, and what was said about it with it. */
  delete_mark(args: {
    m: Mark;
  }): Promise<void>;
  /** Runs `delete_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_markMany(items: DeleteMarkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Says something about a page, or about one of its marks. */
  comment_on(args: {
    p: Page;
    words: string;
    m?: Mark;
  }): Promise<Comment>;
  /** Answers a comment. */
  reply(args: {
    c: Comment;
    words: string;
  }): Promise<Reply>;
  /** Marks a conversation as resolved, or opens it again. */
  resolve_comment(args: {
    c: Comment;
    resolved: boolean;
  }): Promise<void>;
  /** Runs `resolve_comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  resolve_commentMany(items: ResolveCommentArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Rewrites what a comment says. */
  edit_comment(args: {
    c: Comment;
    words: string;
  }): Promise<void>;
  /** Runs `edit_comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  edit_commentMany(items: EditCommentArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes a conversation away; a conversation about a sticky note takes the note with it when it is the last one about it. */
  delete_comment(args: {
    c: Comment;
  }): Promise<void>;
  /** Runs `delete_comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_commentMany(items: DeleteCommentArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes one answer of a conversation. */
  delete_reply(args: {
    c: Comment;
    r: Reply;
  }): Promise<void>;
  /** Runs `delete_reply` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_replyMany(items: DeleteReplyArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Signs a page where the person pressed: the signature they drew, else their name set in the signature's box. */
  place_signature(args: {
    p: Page;
    x: number;
    y: number;
    width?: number;
    height?: number;
  }): Promise<Signature>;
  /** Signs a page with a typed name, in its lower right corner: for the palette, a script and an agent. */
  sign_with_name(args: {
    p: Page;
    signer: string;
  }): Promise<void>;
  /** Runs `sign_with_name` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  sign_with_nameMany(items: SignWithNameArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes a signature off the page. */
  delete_signature(args: {
    s: Signature;
  }): Promise<void>;
  /** Runs `delete_signature` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_signatureMany(items: DeleteSignatureArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Fills a form field with words. */
  fill_field(args: {
    name: string;
    text: string;
  }): Promise<void>;
  /** Runs `fill_field` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  fill_fieldMany(items: FillFieldArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Chooses an option of a form's choice. */
  choose_field(args: {
    name: string;
    choice: string;
  }): Promise<void>;
  /** Runs `choose_field` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  choose_fieldMany(items: ChooseFieldArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Turns a form's box on or off. */
  check_field(args: {
    name: string;
    on: boolean;
    export?: string;
  }): Promise<void>;
  /** Runs `check_field` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  check_fieldMany(items: CheckFieldArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Remembers where the reader left off. */
  remember_reading(args: {
    p: Page;
    fraction: number;
    zoom: number;
  }): Promise<void>;
  /** Runs `remember_reading` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remember_readingMany(items: RememberReadingArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes over the mark-up the PDF's own bytes carry: its highlights, notes with their answers, free text and ink become this document's, once per source. The file's own objects are kept, so a copy saved with them shows each mark once. */
  adopt_markup(): Promise<void>;
  /** Runs `adopt_markup` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  adopt_markupMany(items: AdoptMarkupArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Finds the words `query` in the document's pages: each page that says them, with how often, as `Page 3: 2`. An agent reads a PDF's answer without the window. */
  find_in(args: {
    query: string;
  }): Promise<string[]>;
  /** Copies the words a page shows between two runs, as selecting them and pressing Copy does. */
  copy_selected(args: {
    p: Page;
    from: number;
    to: number;
  }): Promise<void>;
  /** Copies every word a page shows. */
  copy_page(args: {
    p: Page;
  }): Promise<void>;
  /** Saves a copy of the PDF with the reader's mark-up in it: its pages in the reader's order, its marks, what was said about them, the form's values and the file's own mark-up kept where nobody changed it. */
  save_copy(): Promise<unknown>;
  /** Remembers that the reader turned to the page at `at`. */
  turn_to(args: {
    at: number;
    zoom: number;
  }): Promise<void>;
  /** Runs `turn_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  turn_toMany(items: TurnToArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts the page at `index` where the rail dropped it: after the page at `at`. */
  reorder(args: {
    index: number;
    at: number;
  }): Promise<void>;
  /** Runs `reorder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  reorderMany(items: ReorderArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Imports pages after `p`: what the rail's add asks the person for. */
  import_after(args: {
    file: unknown;
    p?: Page;
  }): Promise<Document>;
  /** What pressing on a page does, by the tool the window holds: places a note, a text box or a signature where the person pressed. A marking tool starts a selection instead, which the window keeps. */
  press(args: {
    p: Page;
    at: number[];
    tool: string;
    colour: string;
  }): Promise<void>;
  /** Runs `press` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  pressMany(items: PressArgs[]): Promise<tilo.ActionAnswer[]>;
  /** What letting go does: ink keeps the stroke drawn, and a marking tool marks the words the box covers, or the box itself where the page's words are not read. */
  release(args: {
    p: Page;
    tool: string;
    colour: string;
    stroke: number[];
    start?: number[];
    end?: number[];
  }): Promise<void>;
  /** Runs `release` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  releaseMany(items: ReleaseArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Says something about the page the window shows. */
  speak(args: {
    words: string;
  }): Promise<Comment | null>;
}

/** The actions `Document` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DOCUMENT_ACTIONS = ["lay_out", "lay_outMany", "read_outline", "read_outlineMany", "read_fields", "read_fieldsMany", "read_page", "read_pageMany", "read_links", "read_linksMany", "show_page", "show_pageMany", "read_next", "read_nextMany", "draw_next", "draw_nextMany", "rotate_page", "rotate_pageMany", "move_page", "move_pageMany", "duplicate_page", "delete_page", "delete_pageMany", "import_pages", "import_pagesMany", "mark_text", "draw_ink", "new_note", "add_note", "add_noteMany", "new_text_box", "write_mark", "write_markMany", "move_mark", "move_markMany", "recolour", "recolourMany", "delete_mark", "delete_markMany", "comment_on", "reply", "resolve_comment", "resolve_commentMany", "edit_comment", "edit_commentMany", "delete_comment", "delete_commentMany", "delete_reply", "delete_replyMany", "place_signature", "sign_with_name", "sign_with_nameMany", "delete_signature", "delete_signatureMany", "fill_field", "fill_fieldMany", "choose_field", "choose_fieldMany", "check_field", "check_fieldMany", "remember_reading", "remember_readingMany", "adopt_markup", "adopt_markupMany", "find_in", "copy_selected", "copy_page", "save_copy", "turn_to", "turn_toMany", "reorder", "reorderMany", "import_after", "press", "pressMany", "release", "releaseMany", "speak"] as const;

/** The `tl.so.tilo.pdf-viewer.Document` kind at `path`. */
export function pdfViewerDocument(ws: tilo.Workspace, path: string): Document {
  return resourceProxy(ws, path, SURFACE, DOCUMENT_ACTIONS) as unknown as Document;
}

/** Page — `tl.so.tilo.pdf-viewer.Page`. */
export interface Page extends tilo.Resource {
  oedigi3h: number;  // source_page
  vphbhfba: string;  // imported
  "5c4rqnpi": string;  // kind
  qxyy4u3k: number;  // width
  bepcv7nh: number;  // height
  vlm7goob: number;  // rotation
  fwoikm62: string;  // words
  qko77qye: number[];  // runs
  mxsaptcd: string[];  // run_words
  kdrgc55v: boolean;  // read
}

/** The actions `Page` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PAGE_ACTIONS = [] as const;

/** The `tl.so.tilo.pdf-viewer.Page` kind at `path`. */
export function page(ws: tilo.Workspace, path: string): Page {
  return resourceProxy(ws, path, SURFACE, PAGE_ACTIONS) as unknown as Page;
}

/** Picture — `tl.so.tilo.pdf-viewer.Picture`. */
export interface Picture extends tilo.Resource {
  e5fx4rry: Page;  // page
  lps7jwgj: number;  // width
  ht6kgmaq: string;  // url
}

/** The actions `Picture` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PICTURE_ACTIONS = [] as const;

/** The `tl.so.tilo.pdf-viewer.Picture` kind at `path`. */
export function picture(ws: tilo.Workspace, path: string): Picture {
  return resourceProxy(ws, path, SURFACE, PICTURE_ACTIONS) as unknown as Picture;
}

/** Outline — `tl.so.tilo.pdf-viewer.Outline`. */
export interface Outline extends tilo.Resource {
  qfml2pla: string;  // title
  "34ukafjp": number;  // page
  v7hjvwyk: number;  // level
}

/** The actions `Outline` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OUTLINE_ACTIONS = [] as const;

/** The `tl.so.tilo.pdf-viewer.Outline` kind at `path`. */
export function outline(ws: tilo.Workspace, path: string): Outline {
  return resourceProxy(ws, path, SURFACE, OUTLINE_ACTIONS) as unknown as Outline;
}

/** Link — `tl.so.tilo.pdf-viewer.Link`. */
export interface Link extends tilo.Resource {
  xivruibf: Page;  // page
  sepq4iwp: number[];  // rect
  yfuvjgjt: string;  // kind
  lzpsucsi: string;  // to
}

/** The actions `Link` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LINK_ACTIONS = [] as const;

/** The `tl.so.tilo.pdf-viewer.Link` kind at `path`. */
export function link(ws: tilo.Workspace, path: string): Link {
  return resourceProxy(ws, path, SURFACE, LINK_ACTIONS) as unknown as Link;
}

/** Field — `tl.so.tilo.pdf-viewer.Field`. */
export interface Field extends tilo.Resource {
  "5ghgpzwq": string;  // name
  "4mifdrqw": string;  // kind
  enpzi5ta: string;  // value
  f6evcr4f: boolean;  // checked
  jjifolfg: string;  // export
  ya7ugvvf: string[];  // options
  h2kxabyf: number;  // page
  ijs3t4gj: number[];  // rect
  eh3gs42n: boolean;  // read_only
  lxh5hh6y: boolean;  // required
}

/** The actions `Field` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FIELD_ACTIONS = [] as const;

/** The `tl.so.tilo.pdf-viewer.Field` kind at `path`. */
export function field(ws: tilo.Workspace, path: string): Field {
  return resourceProxy(ws, path, SURFACE, FIELD_ACTIONS) as unknown as Field;
}

/** Mark — `tl.so.tilo.pdf-viewer.Mark`. */
export interface Mark extends tilo.Resource {
  "4x7a7juf": Page;  // page
  n23wswru: MarkKind;  // kind
  ikltq7t2: string;  // colour
  "35epyc2h": string;  // style
  qtn6mlbl: number[];  // rects
  crim3nye: number;  // x
  "3rusqrrq": number;  // y
  agxxncq4: string;  // words
  xt4alsjd: number;  // size
  fz2kryor: Array<tilo.Entry<Stroke>>;  // strokes
  "2a5limjf": string;  // author
  fkkbarxd: string;  // adopted_id
}

/** The actions `Mark` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MARK_ACTIONS = [] as const;

/** The `tl.so.tilo.pdf-viewer.Mark` kind at `path`. */
export function mark(ws: tilo.Workspace, path: string): Mark {
  return resourceProxy(ws, path, SURFACE, MARK_ACTIONS) as unknown as Mark;
}

/** Stroke — `tl.so.tilo.pdf-viewer.Stroke`. */
export interface Stroke extends tilo.Resource {
  "4dzcakfh": number[];  // points
}

/** The actions `Stroke` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const STROKE_ACTIONS = [] as const;

/** The `tl.so.tilo.pdf-viewer.Stroke` kind at `path`. */
export function stroke(ws: tilo.Workspace, path: string): Stroke {
  return resourceProxy(ws, path, SURFACE, STROKE_ACTIONS) as unknown as Stroke;
}

/** Comment — `tl.so.tilo.pdf-viewer.Comment`. */
export interface Comment extends tilo.Resource {
  e5qxkctu: Mark | null;  // mark
  btdnd5pa: Page;  // page
  ogqe2kdm: string;  // words
  hcfcbfso: string;  // body
  hgps2s5i: boolean;  // resolved
  thfbylkn: Array<tilo.Entry<Reply>>;  // replies
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
}

/** The actions `Comment` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const COMMENT_ACTIONS = ["editBlock", "editBlockMany", "insertBlock", "insertBlockMany", "deleteBlock", "deleteBlockMany"] as const;

/** The `tl.so.tilo.pdf-viewer.Comment` kind at `path`. */
export function comment(ws: tilo.Workspace, path: string): Comment {
  return resourceProxy(ws, path, SURFACE, COMMENT_ACTIONS) as unknown as Comment;
}

/** Reply — `tl.so.tilo.pdf-viewer.Reply`. */
export interface Reply extends tilo.Resource {
  "47zrerme": string;  // words
  exvjyq6h: string;  // body
  /** Set one paragraph of a rich-text field to what a Markdown text says. */
  editBlock(args: {
    block: string;
    text: string;
  }): Promise<void>;
  /** Runs `edit-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  editBlockMany(items: ReplyEditBlockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Put a paragraph before the one the address names. */
  insertBlock(args: {
    block: string;
    text: string;
  }): Promise<void>;
  /** Runs `insert-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  insertBlockMany(items: ReplyInsertBlockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Remove the paragraph the address names, and the paragraphs it holds. */
  deleteBlock(args: {
    block: string;
  }): Promise<void>;
  /** Runs `delete-block` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteBlockMany(items: ReplyDeleteBlockArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Reply` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REPLY_ACTIONS = ["editBlock", "editBlockMany", "insertBlock", "insertBlockMany", "deleteBlock", "deleteBlockMany"] as const;

/** The `tl.so.tilo.pdf-viewer.Reply` kind at `path`. */
export function reply(ws: tilo.Workspace, path: string): Reply {
  return resourceProxy(ws, path, SURFACE, REPLY_ACTIONS) as unknown as Reply;
}

/** Signature — `tl.so.tilo.pdf-viewer.Signature`. */
export interface Signature extends tilo.Resource {
  xudq4utx: Page;  // page
  uyofemys: number;  // x
  mai2dzre: number;  // y
  tvp2usjk: number;  // width
  iriyo77n: number;  // height
  ozrikra3: string;  // signer
  k6r3zcgg: string;  // date
  "5pbiosiz": Array<tilo.Entry<Stroke>>;  // strokes
}

/** The actions `Signature` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SIGNATURE_ACTIONS = [] as const;

/** The `tl.so.tilo.pdf-viewer.Signature` kind at `path`. */
export function signature(ws: tilo.Workspace, path: string): Signature {
  return resourceProxy(ws, path, SURFACE, SIGNATURE_ACTIONS) as unknown as Signature;
}

/** Library — `tl.so.tilo.pdf-viewer.Library`. */
export interface Library extends tilo.Resource {
  bqmqo56y: Array<tilo.Entry<Document>>;  // documents
  u2q7brpp: string;  // signer
  bny56wzk: Array<tilo.Entry<Stroke>>;  // strokes
  /** A PDF opens in the reader: Files, the person's own file system and the system's search hand it over as the file it is, and it opens on the document those bytes already are. */
  open_pdf(args: {
    file: unknown;
  }): Promise<Document>;
  /** Opens the PDF whose bytes the workspace keeps at `address`: the document those bytes are if one holds them, else a new one. How an agent, a link or another app opens a PDF. */
  open_source(args: {
    address: string;
    name?: string;
  }): Promise<Document>;
  /** The name typed signatures carry from now on. */
  sign_as(args: {
    name: string;
  }): Promise<void>;
  /** Runs `sign_as` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  sign_asMany(items: SignAsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps the signature a person drew, so the next document is signed with one press: the strokes drawn on the pad, x and y in turn in fractions of the signature's box. */
  draw_signature(args: {
    strokes: number[][];
  }): Promise<void>;
  /** Runs `draw_signature` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  draw_signatureMany(items: DrawSignatureArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Library` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LIBRARY_ACTIONS = ["open_pdf", "open_source", "sign_as", "sign_asMany", "draw_signature", "draw_signatureMany"] as const;

/** The `tl.so.tilo.pdf-viewer.Library` kind at `path`. */
export function library(ws: tilo.Workspace, path: string): Library {
  return resourceProxy(ws, path, SURFACE, LIBRARY_ACTIONS) as unknown as Library;
}

/** Runs — `tl.so.tilo.pdf-viewer.Runs`. */
export interface Runs extends tilo.Resource {
  fmkeszhi: number[];  // rects
  awfhznwx: string[];  // texts
}

/** The actions `Runs` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RUNS_ACTIONS = [] as const;

/** The `tl.so.tilo.pdf-viewer.Runs` kind at `path`. */
export function runs(ws: tilo.Workspace, path: string): Runs {
  return resourceProxy(ws, path, SURFACE, RUNS_ACTIONS) as unknown as Runs;
}

/** PageRow — `tl.so.tilo.pdf-viewer.PageRow`. */
export interface PageRow extends tilo.Resource {
  "2xoqlfit": number;  // page
  hsgsnasu: number;  // width
  vmsxv3ma: number;  // height
}

/** The actions `PageRow` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PAGEROW_ACTIONS = [] as const;

/** The `tl.so.tilo.pdf-viewer.PageRow` kind at `path`. */
export function pageRow(ws: tilo.Workspace, path: string): PageRow {
  return resourceProxy(ws, path, SURFACE, PAGEROW_ACTIONS) as unknown as PageRow;
}

/** GlyphRow — `tl.so.tilo.pdf-viewer.GlyphRow`. */
export interface GlyphRow extends tilo.Resource {
  e35nwhcd: string;  // text
  owthh7c4: number;  // line
  at7kirp3: number;  // x
  w53k2oqb: number;  // y
  "7ufljz44": number;  // w
  showgoq3: number;  // h
  "5u27ffde": boolean;  // placed
}

/** The actions `GlyphRow` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const GLYPHROW_ACTIONS = [] as const;

/** The `tl.so.tilo.pdf-viewer.GlyphRow` kind at `path`. */
export function glyphRow(ws: tilo.Workspace, path: string): GlyphRow {
  return resourceProxy(ws, path, SURFACE, GLYPHROW_ACTIONS) as unknown as GlyphRow;
}

/** ImportRow — `tl.so.tilo.pdf-viewer.ImportRow`. */
export interface ImportRow extends tilo.Resource {
  b35hfv2r: string;  // kind
  m2v5uip7: PageRow[];  // pages
}

/** The actions `ImportRow` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const IMPORTROW_ACTIONS = [] as const;

/** The `tl.so.tilo.pdf-viewer.ImportRow` kind at `path`. */
export function importRow(ws: tilo.Workspace, path: string): ImportRow {
  return resourceProxy(ws, path, SURFACE, IMPORTROW_ACTIONS) as unknown as ImportRow;
}

/** OutlineRow — `tl.so.tilo.pdf-viewer.OutlineRow`. */
export interface OutlineRow extends tilo.Resource {
  rl5gr6nc: string;  // title
  mtayx6nb: number;  // page
  ewudh3r2: number;  // level
}

/** The actions `OutlineRow` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OUTLINEROW_ACTIONS = [] as const;

/** The `tl.so.tilo.pdf-viewer.OutlineRow` kind at `path`. */
export function outlineRow(ws: tilo.Workspace, path: string): OutlineRow {
  return resourceProxy(ws, path, SURFACE, OUTLINEROW_ACTIONS) as unknown as OutlineRow;
}

/** MarkRow — `tl.so.tilo.pdf-viewer.MarkRow`. */
export interface MarkRow extends tilo.Resource {
  s3btzxsu: number;  // page
  "5x3rjw33": number;  // index
  l6esysss: string;  // name
  i3qsks26: string;  // kind
  qca4t246: string | null;  // color
  "5ddf4ghi": string;  // contents
  ejgwsaae: string;  // author
  "4chyadse": string;  // created
  sw7xlda5: string;  // modified
  xsmehh7m: number[];  // rect
  v7q3ceeu: number[][];  // quads
  pv225oqp: number[][];  // strokes
  yrxpeq4o: number | null;  // size
  xkarfjs4: number | null;  // in_reply_to
  rwsq3hzc: boolean;  // reply
  udw6i6fp: boolean;  // hidden
  nuljshqh: boolean;  // readable
}

/** The actions `MarkRow` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MARKROW_ACTIONS = [] as const;

/** The `tl.so.tilo.pdf-viewer.MarkRow` kind at `path`. */
export function markRow(ws: tilo.Workspace, path: string): MarkRow {
  return resourceProxy(ws, path, SURFACE, MARKROW_ACTIONS) as unknown as MarkRow;
}

/** MarkupRow — `tl.so.tilo.pdf-viewer.MarkupRow`. */
export interface MarkupRow extends tilo.Resource {
  "6cyewmj4": MarkRow[];  // marks
  dk6bxvq5: FieldRow[];  // fields
}

/** The actions `MarkupRow` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MARKUPROW_ACTIONS = [] as const;

/** The `tl.so.tilo.pdf-viewer.MarkupRow` kind at `path`. */
export function markupRow(ws: tilo.Workspace, path: string): MarkupRow {
  return resourceProxy(ws, path, SURFACE, MARKUPROW_ACTIONS) as unknown as MarkupRow;
}

/** FieldRow — `tl.so.tilo.pdf-viewer.FieldRow`. */
export interface FieldRow extends tilo.Resource {
  havty2vp: string;  // name
  d4cnpeq3: string;  // kind
  br4tfro7: string;  // value
  jyx27y2l: boolean;  // checked
  tfvicybt: string;  // export
  "66vdxlss": string[];  // options
  puoybohy: number | null;  // page
  ip3ujn6w: number[] | null;  // rect
  mneasolx: boolean;  // read_only
  i46gksbq: boolean;  // required
}

/** The actions `FieldRow` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FIELDROW_ACTIONS = [] as const;

/** The `tl.so.tilo.pdf-viewer.FieldRow` kind at `path`. */
export function fieldRow(ws: tilo.Workspace, path: string): FieldRow {
  return resourceProxy(ws, path, SURFACE, FIELDROW_ACTIONS) as unknown as FieldRow;
}

/** PageOut — `tl.so.tilo.pdf-viewer.PageOut`. */
export interface PageOut extends tilo.Resource {
  d56ntmtj: number;  // source
  "5y2litv2": number;  // page
  hgb4qbox: number;  // rotation
  oaf2yyke: boolean;  // primary
}

/** The actions `PageOut` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PAGEOUT_ACTIONS = [] as const;

/** The `tl.so.tilo.pdf-viewer.PageOut` kind at `path`. */
export function pageOut(ws: tilo.Workspace, path: string): PageOut {
  return resourceProxy(ws, path, SURFACE, PAGEOUT_ACTIONS) as unknown as PageOut;
}

/** MarkOut — `tl.so.tilo.pdf-viewer.MarkOut`. */
export interface MarkOut extends tilo.Resource {
  qlypcvpd: number;  // page
  jfdwouoh: string;  // kind
  "4aptjhj3": string | null;  // color
  "5vi7wvna": string;  // author
  wt44cnbi: string;  // contents
  cv6deozk: string;  // name
  uzy5hqqe: number;  // created_ms
  qnpoauzu: number[][];  // rects
  ilro56v5: number[][];  // strokes
  slisyq2r: number[] | null;  // rect
  u3zrbin4: number | null;  // size
}

/** The actions `MarkOut` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MARKOUT_ACTIONS = [] as const;

/** The `tl.so.tilo.pdf-viewer.MarkOut` kind at `path`. */
export function markOut(ws: tilo.Workspace, path: string): MarkOut {
  return resourceProxy(ws, path, SURFACE, MARKOUT_ACTIONS) as unknown as MarkOut;
}

/** ReplyOut — `tl.so.tilo.pdf-viewer.ReplyOut`. */
export interface ReplyOut extends tilo.Resource {
  "7ckypqja": number;  // page
  ksvfd5se: number;  // x
  g6nqwvtx: number;  // y
  qs6kxe3w: string;  // author
  vqovoaba: string;  // body
  kcbfphy7: string;  // name
  qdwom5mm: string;  // answers
}

/** The actions `ReplyOut` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REPLYOUT_ACTIONS = [] as const;

/** The `tl.so.tilo.pdf-viewer.ReplyOut` kind at `path`. */
export function replyOut(ws: tilo.Workspace, path: string): ReplyOut {
  return resourceProxy(ws, path, SURFACE, REPLYOUT_ACTIONS) as unknown as ReplyOut;
}

/** ValueOut — `tl.so.tilo.pdf-viewer.ValueOut`. */
export interface ValueOut extends tilo.Resource {
  "4uvljgge": string;  // name
  tlqdrzox: string;  // kind
  fp6projh: string;  // text
  oquhk6ja: boolean;  // on
  mlfgloi5: string | null;  // export
}

/** The actions `ValueOut` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const VALUEOUT_ACTIONS = [] as const;

/** The `tl.so.tilo.pdf-viewer.ValueOut` kind at `path`. */
export function valueOut(ws: tilo.Workspace, path: string): ValueOut {
  return resourceProxy(ws, path, SURFACE, VALUEOUT_ACTIONS) as unknown as ValueOut;
}

/** AdoptedOut — `tl.so.tilo.pdf-viewer.AdoptedOut`. */
export interface AdoptedOut extends tilo.Resource {
  yzggzvtt: number;  // page
  jxgttint: number;  // index
  k3tlktwg: string;  // name
  ua2gidzw: string;  // kind
  jlsyhy65: boolean;  // unchanged
}

/** The actions `AdoptedOut` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ADOPTEDOUT_ACTIONS = [] as const;

/** The `tl.so.tilo.pdf-viewer.AdoptedOut` kind at `path`. */
export function adoptedOut(ws: tilo.Workspace, path: string): AdoptedOut {
  return resourceProxy(ws, path, SURFACE, ADOPTEDOUT_ACTIONS) as unknown as AdoptedOut;
}

/** SaveRequest — `tl.so.tilo.pdf-viewer.SaveRequest`. */
export interface SaveRequest extends tilo.Resource {
  l5cfhg2u: PageOut[];  // pages
  egrqnnmk: MarkOut[];  // marks
  fy7mbgs7: ReplyOut[];  // replies
  q2jlnwmi: ValueOut[];  // values
  igdolmln: AdoptedOut[];  // adopted
}

/** The actions `SaveRequest` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SAVEREQUEST_ACTIONS = [] as const;

/** The `tl.so.tilo.pdf-viewer.SaveRequest` kind at `path`. */
export function saveRequest(ws: tilo.Workspace, path: string): SaveRequest {
  return resourceProxy(ws, path, SURFACE, SAVEREQUEST_ACTIONS) as unknown as SaveRequest;
}

/** MarkColour — `tl.so.tilo.pdf-viewer.MarkColour`. */
export interface MarkColour extends tilo.Resource {
  yr5hwqqr: string;  // name
  fmr3zdmh: string;  // value
}

/** The actions `MarkColour` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MARKCOLOUR_ACTIONS = [] as const;

/** The `tl.so.tilo.pdf-viewer.MarkColour` kind at `path`. */
export function markColour(ws: tilo.Workspace, path: string): MarkColour {
  return resourceProxy(ws, path, SURFACE, MARKCOLOUR_ACTIONS) as unknown as MarkColour;
}

/** PDF Viewer — `tl.so.tilo.pdf-viewer`. */
export interface PdfViewer extends tilo.Resource {
}

/** The actions `PdfViewer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PDFVIEWER_ACTIONS = [] as const;

/** The `tl.so.tilo.pdf-viewer` kind at `path`. */
export function pdfViewer(ws: tilo.Workspace, path: string): PdfViewer {
  return resourceProxy(ws, path, SURFACE, PDFVIEWER_ACTIONS) as unknown as PdfViewer;
}

