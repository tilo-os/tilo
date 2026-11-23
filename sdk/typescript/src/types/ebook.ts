/**
 * Types for Books (`so.tilo.ebook`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Book` is a `Resource` of the app's `tl.so.tilo.ebook.Book`, made by its factory with the
 * resource's path (`ebook/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.ebook@3b10ed27c5684dba";

/** The arguments of `remove_book`. */
export interface RemoveBookArgs {
}

/** The arguments of `record_at`. */
export interface RecordAtArgs {
  who: string;
  chapter: number;
  block: number;
  page: number;
  place: number;
}

/** The arguments of `read_chapter`. */
export interface ReadChapterArgs {
  who: string;
  c: Chapter;
}

/** The arguments of `show_page`. */
export interface ShowPageArgs {
  who: string;
  chapter: number;
  block: number;
  page: number;
}

/** The arguments of `show_passage`. */
export interface ShowPassageArgs {
  who: string;
  chapter: number;
  block: number;
}

/** The arguments of `show_place`. */
export interface ShowPlaceArgs {
  who: string;
  chapter: number;
  block: number;
  page: number;
  place: number;
}

/** The arguments of `saw_blocks`. */
export interface SawBlocksArgs {
  who: string;
  blocks: number[];
}

/** The arguments of `highlight`. */
export interface HighlightArgs {
  chapter: number;
  ranges: number[];
  color: string;
}

/** The arguments of `add_note`. */
export interface AddNoteArgs {
  chapter: number;
  ranges: number[];
  color: string;
  text: string;
}

/** The arguments of `toggle_bookmark`. */
export interface ToggleBookmarkArgs {
  chapter: number;
  block: number;
}

/** The arguments of `comment`. */
export interface CommentArgs {
  m: Mark;
  text: string;
}

/** The arguments of `change_color`. */
export interface ChangeColorArgs {
  m: Mark;
  color: string;
}

/** The arguments of `delete_mark`. */
export interface DeleteMarkArgs {
  m: Mark;
}

/** The arguments of `drop_books`. */
export interface DropBooksArgs {
  files: unknown[];
}

/** The arguments of `set_text_size`. */
export interface SetTextSizeArgs {
  size: number;
}

/** The arguments of `set_line_spacing`. */
export interface SetLineSpacingArgs {
  spacing: number;
}

/** The arguments of `set_face`. */
export interface SetFaceArgs {
  face: string;
}

/** The arguments of `set_justified`. */
export interface SetJustifiedArgs {
  on: boolean;
}

/** The arguments of `cycle_scheme`. */
export interface CycleSchemeArgs {
}

/** The arguments of `set_scheme`. */
export interface SetSchemeArgs {
  name: string;
}

/** The arguments of `add_scheme`. */
export interface AddSchemeArgs {
  name: string;
  page: string;
  ink: string;
}

/** The arguments of `delete_scheme`. */
export interface DeleteSchemeArgs {
  scheme: Scheme;
}

/** The arguments of `find_books`. */
export interface FindBooksArgs {
  query: string;
}

/** The arguments of `download_found`. */
export interface DownloadFoundArgs {
  f: Found;
}

/** The arguments of `clear_found`. */
export interface ClearFoundArgs {
}

/** Book — `tl.so.tilo.ebook.Book`. */
export interface Book extends tilo.Resource {
  zzi6jbdk: string;  // title
  hzsemqyf: string;  // author
  ywdncocc: string;  // subject
  nczhbvr6: string;  // language
  uufjkgr3: string;  // source
  t7fdn6od: string;  // cover
  sthxrdo4: string;  // added
  s4t4ogtx: Array<tilo.Entry<Chapter>>;  // chapters
  kckryjj5: Array<tilo.Entry<Mark>>;  // marks
  y3qlwotc: Array<tilo.Entry<Progress>>;  // progress
  /** Takes a book off the shelf, its chapters and marks with it. Reading it again adds it again from its file. */
  remove_book(): Promise<void>;
  /** Runs `remove_book` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_bookMany(items: RemoveBookArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Where the person reading is in `b`: their own record, changed in place so its history says where they read and when. */
  record_at(args: {
    who: string;
    chapter: number;
    block: number;
    page: number;
    place: number;
  }): Promise<void>;
  /** Runs `record_at` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  record_atMany(items: RecordAtArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens `c` in `b`: the contents and where the person is are kept together. */
  read_chapter(args: {
    who: string;
    c: Chapter;
  }): Promise<void>;
  /** Runs `read_chapter` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  read_chapterMany(items: ReadChapterArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows page `page` of the book's open chapter, counted from 0 (a spread in the two-page mode). */
  show_page(args: {
    who: string;
    chapter: number;
    block: number;
    page: number;
  }): Promise<void>;
  /** Runs `show_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  show_pageMany(items: ShowPageArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows the first page of the chapter `chapter` in `b`: a link to a passage lands on it. */
  show_passage(args: {
    who: string;
    chapter: number;
    block: number;
  }): Promise<void>;
  /** Runs `show_passage` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  show_passageMany(items: ShowPassageArgs[]): Promise<tilo.ActionAnswer[]>;
  /** How far down the column the person scrolled. */
  show_place(args: {
    who: string;
    chapter: number;
    block: number;
    page: number;
    place: number;
  }): Promise<void>;
  /** Runs `show_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  show_placeMany(items: ShowPlaceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Notes the block the page begins at as the person scrolls the column, so a bookmark lands where they are. */
  saw_blocks(args: {
    who: string;
    blocks: number[];
  }): Promise<void>;
  /** Runs `saw_blocks` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  saw_blocksMany(items: SawBlocksArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Highlights the passage of `b`'s open chapter: its ranges, each `block, start, end`. */
  highlight(args: {
    chapter: number;
    ranges: number[];
    color: string;
  }): Promise<void>;
  /** Runs `highlight` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  highlightMany(items: HighlightArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Writes a note on the passage of `b`'s open chapter. */
  add_note(args: {
    chapter: number;
    ranges: number[];
    color: string;
    text: string;
  }): Promise<void>;
  /** Runs `add_note` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  add_noteMany(items: AddNoteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Bookmarks where the page `block` begins in `b`'s open chapter, or takes the bookmark off again. */
  toggle_bookmark(args: {
    chapter: number;
    block: number;
  }): Promise<void>;
  /** Runs `toggle_bookmark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  toggle_bookmarkMany(items: ToggleBookmarkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Comments on the highlight `m`, in the book `b`. */
  comment(args: {
    m: Mark;
    text: string;
  }): Promise<void>;
  /** Runs `comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  commentMany(items: CommentArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Recolours a highlight, and the notes on it. */
  change_color(args: {
    m: Mark;
    color: string;
  }): Promise<void>;
  /** Runs `change_color` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  change_colorMany(items: ChangeColorArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes a mark, and the comments on it. */
  delete_mark(args: {
    m: Mark;
  }): Promise<void>;
  /** Runs `delete_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_markMany(items: DeleteMarkArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Book` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const BOOK_ACTIONS = ["remove_book", "remove_bookMany", "record_at", "record_atMany", "read_chapter", "read_chapterMany", "show_page", "show_pageMany", "show_passage", "show_passageMany", "show_place", "show_placeMany", "saw_blocks", "saw_blocksMany", "highlight", "highlightMany", "add_note", "add_noteMany", "toggle_bookmark", "toggle_bookmarkMany", "comment", "commentMany", "change_color", "change_colorMany", "delete_mark", "delete_markMany"] as const;

/** The `tl.so.tilo.ebook.Book` kind at `path`. */
export function book(ws: tilo.Workspace, path: string): Book {
  return resourceProxy(ws, path, SURFACE, BOOK_ACTIONS) as unknown as Book;
}

/** Chapter — `tl.so.tilo.ebook.Chapter`. */
export interface Chapter extends tilo.Resource {
  njsapppc: string;  // title
  hc6oa77y: string;  // body
  awonaulm: Book | null;  // book
}

/** The actions `Chapter` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CHAPTER_ACTIONS = [] as const;

/** The `tl.so.tilo.ebook.Chapter` kind at `path`. */
export function chapter(ws: tilo.Workspace, path: string): Chapter {
  return resourceProxy(ws, path, SURFACE, CHAPTER_ACTIONS) as unknown as Chapter;
}

/** Mark — `tl.so.tilo.ebook.Mark`. */
export interface Mark extends tilo.Resource {
  nsx4szpu: string;  // kind
  eu36rgok: number;  // chapter
  kkwkq4sx: number;  // block
  c7gbrwjv: number;  // start
  gwydxwhz: number;  // end
  pjwr2noa: string;  // excerpt
  jlprlqyv: string;  // color
  q6o2tid6: string;  // text
  qzbrg6g6: string;  // created
  jt22d5yp: string;  // on
  gjbrlgkj: Book | null;  // book
  vpgm2esg: Piece[];  // pieces
}

/** The actions `Mark` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MARK_ACTIONS = [] as const;

/** The `tl.so.tilo.ebook.Mark` kind at `path`. */
export function mark(ws: tilo.Workspace, path: string): Mark {
  return resourceProxy(ws, path, SURFACE, MARK_ACTIONS) as unknown as Mark;
}

/** Piece — `tl.so.tilo.ebook.Piece`. */
export interface Piece extends tilo.Resource {
  el2sirvc: number;  // block
  wjiavs4p: number;  // start
  mk52mcqy: number;  // end
}

/** The actions `Piece` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PIECE_ACTIONS = [] as const;

/** The `tl.so.tilo.ebook.Piece` kind at `path`. */
export function piece(ws: tilo.Workspace, path: string): Piece {
  return resourceProxy(ws, path, SURFACE, PIECE_ACTIONS) as unknown as Piece;
}

/** Progress — `tl.so.tilo.ebook.Progress`. */
export interface Progress extends tilo.Resource {
  fw6cx373: string;  // who
  hrlsiabz: number;  // chapter
  dozgglst: number;  // block
  j7r2dpph: number;  // page
  t4pdkbb6: number;  // place
  vpcehkho: string;  // at
}

/** The actions `Progress` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PROGRESS_ACTIONS = [] as const;

/** The `tl.so.tilo.ebook.Progress` kind at `path`. */
export function progress(ws: tilo.Workspace, path: string): Progress {
  return resourceProxy(ws, path, SURFACE, PROGRESS_ACTIONS) as unknown as Progress;
}

/** Scheme — `tl.so.tilo.ebook.Scheme`. */
export interface Scheme extends tilo.Resource {
  yjaxgs5w: string;  // name
  zfe44ecr: string;  // page
  giw4wwwc: string;  // ink
  p6cg5shx: string;  // accent
  "2xhwxzlq": boolean;  // builtin
}

/** The actions `Scheme` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SCHEME_ACTIONS = [] as const;

/** The `tl.so.tilo.ebook.Scheme` kind at `path`. */
export function scheme(ws: tilo.Workspace, path: string): Scheme {
  return resourceProxy(ws, path, SURFACE, SCHEME_ACTIONS) as unknown as Scheme;
}

/** Found — `tl.so.tilo.ebook.Found`. */
export interface Found extends tilo.Resource {
  cuehsj4a: string;  // title
  "5z23zsgs": string;  // author
  "5kd3qulh": string;  // url
  rgyfsse6: string;  // language
  fhfwqqv3: string;  // query
}

/** The actions `Found` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FOUND_ACTIONS = [] as const;

/** The `tl.so.tilo.ebook.Found` kind at `path`. */
export function found(ws: tilo.Workspace, path: string): Found {
  return resourceProxy(ws, path, SURFACE, FOUND_ACTIONS) as unknown as Found;
}

/** Library — `tl.so.tilo.ebook.Library`. */
export interface Library extends tilo.Resource {
  rjqqjeka: Array<tilo.Entry<Book>>;  // books
  gark455v: Array<tilo.Entry<Found>>;  // found
  b2rhddat: Array<tilo.Entry<Scheme>>;  // schemes
  sndaeoib: number;  // text_size
  "3fnv4dcf": number;  // line_spacing
  "5umaklng": string;  // face
  "4nrbbruw": boolean;  // justified
  q2xeecyu: string;  // scheme
  "7iepiv5z": string;  // draft_scheme
  wc5z47pj: string;  // custom_page
  yiozk22e: string;  // custom_ink
  /** Adds the EPUB `file` to the shelf and opens it. Its title, author and language are the book's own; each chapter becomes a resource holding its words, so the book is searchable and a reader opens the chapter they are at. */
  open_book(args: {
    file: unknown;
  }): Promise<Book>;
  /** Adds the EPUB files a person dropped on the shelf, each as `open_book` adds it. */
  drop_books(args: {
    files: unknown[];
  }): Promise<void>;
  /** Runs `drop_books` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  drop_booksMany(items: DropBooksArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets the page's text size, within what a page can be read at. */
  set_text_size(args: {
    size: number;
  }): Promise<void>;
  /** Runs `set_text_size` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_text_sizeMany(items: SetTextSizeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets the page's line spacing. */
  set_line_spacing(args: {
    spacing: number;
  }): Promise<void>;
  /** Runs `set_line_spacing` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_line_spacingMany(items: SetLineSpacingArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets the face the page is set in. */
  set_face(args: {
    face: string;
  }): Promise<void>;
  /** Runs `set_face` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_faceMany(items: SetFaceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Whether the page's paragraphs are justified. */
  set_justified(args: {
    on: boolean;
  }): Promise<void>;
  /** Runs `set_justified` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_justifiedMany(items: SetJustifiedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Steps to the next palette the reader ships with: its bar's quick switch. */
  cycle_scheme(): Promise<void>;
  /** Runs `cycle_scheme` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  cycle_schemeMany(items: CycleSchemeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The palette the page is set in, by name. */
  set_scheme(args: {
    name: string;
  }): Promise<void>;
  /** Runs `set_scheme` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_schemeMany(items: SetSchemeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** A palette of the person's own, from a paper and an ink colour. */
  add_scheme(args: {
    name: string;
    page: string;
    ink: string;
  }): Promise<void>;
  /** Runs `add_scheme` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  add_schemeMany(items: AddSchemeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes a palette of the person's own. The four the reader ships with stay. */
  delete_scheme(args: {
    scheme: Scheme;
  }): Promise<void>;
  /** Runs `delete_scheme` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_schemeMany(items: DeleteSchemeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Searches Project Gutenberg for books matching `query`, taking the answers an earlier search left behind. The books stand under the shelf until a person downloads one. */
  find_books(args: {
    query: string;
  }): Promise<void>;
  /** Runs `find_books` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  find_booksMany(items: FindBooksArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Downloads the book an online search found and opens it. */
  download_found(args: {
    f: Found;
  }): Promise<void>;
  /** Runs `download_found` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  download_foundMany(items: DownloadFoundArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes an online search's answers away. */
  clear_found(): Promise<void>;
  /** Runs `clear_found` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_foundMany(items: ClearFoundArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Library` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LIBRARY_ACTIONS = ["open_book", "drop_books", "drop_booksMany", "set_text_size", "set_text_sizeMany", "set_line_spacing", "set_line_spacingMany", "set_face", "set_faceMany", "set_justified", "set_justifiedMany", "cycle_scheme", "cycle_schemeMany", "set_scheme", "set_schemeMany", "add_scheme", "add_schemeMany", "delete_scheme", "delete_schemeMany", "find_books", "find_booksMany", "download_found", "download_foundMany", "clear_found", "clear_foundMany"] as const;

/** The `tl.so.tilo.ebook.Library` kind at `path`. */
export function library(ws: tilo.Workspace, path: string): Library {
  return resourceProxy(ws, path, SURFACE, LIBRARY_ACTIONS) as unknown as Library;
}

/** FoundAuthor — `tl.so.tilo.ebook.FoundAuthor`. */
export interface FoundAuthor extends tilo.Resource {
  j3ucstoe: string;  // name
}

/** The actions `FoundAuthor` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FOUNDAUTHOR_ACTIONS = [] as const;

/** The `tl.so.tilo.ebook.FoundAuthor` kind at `path`. */
export function foundAuthor(ws: tilo.Workspace, path: string): FoundAuthor {
  return resourceProxy(ws, path, SURFACE, FOUNDAUTHOR_ACTIONS) as unknown as FoundAuthor;
}

/** FoundBook — `tl.so.tilo.ebook.FoundBook`. */
export interface FoundBook extends tilo.Resource {
  v7nyajgy: string;  // title
  a4rek4qh: FoundAuthor[] | null;  // authors
  "7uazgom7": string[] | null;  // languages
  uw6rgipi: Record<string, string> | null;  // formats
}

/** The actions `FoundBook` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FOUNDBOOK_ACTIONS = [] as const;

/** The `tl.so.tilo.ebook.FoundBook` kind at `path`. */
export function foundBook(ws: tilo.Workspace, path: string): FoundBook {
  return resourceProxy(ws, path, SURFACE, FOUNDBOOK_ACTIONS) as unknown as FoundBook;
}

/** FoundAnswer — `tl.so.tilo.ebook.FoundAnswer`. */
export interface FoundAnswer extends tilo.Resource {
  ngjj6bkr: FoundBook[];  // results
}

/** The actions `FoundAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FOUNDANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.ebook.FoundAnswer` kind at `path`. */
export function foundAnswer(ws: tilo.Workspace, path: string): FoundAnswer {
  return resourceProxy(ws, path, SURFACE, FOUNDANSWER_ACTIONS) as unknown as FoundAnswer;
}

/** Books — `tl.so.tilo.ebook`. */
export interface Ebook extends tilo.Resource {
}

/** The actions `Ebook` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const EBOOK_ACTIONS = [] as const;

/** The `tl.so.tilo.ebook` kind at `path`. */
export function ebook(ws: tilo.Workspace, path: string): Ebook {
  return resourceProxy(ws, path, SURFACE, EBOOK_ACTIONS) as unknown as Ebook;
}

