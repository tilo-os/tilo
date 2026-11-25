/**
 * Types for Typst (`so.tilo.typst-editor`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`typst-editor/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.typst-editor@2cd7d0d73af29dc5";

/** How much a problem the typesetter found matters. One of "error", "warning". */
export type Severity = "error" | "warning";

/** The arguments of `typeset`. */
export interface TypesetArgs {
}

/** The arguments of `typeset_if_stale`. */
export interface TypesetIfStaleArgs {
}

/** The arguments of `take_problems`. */
export interface TakeProblemsArgs {
  rows: ProblemRow[];
}

/** The arguments of `take_pages`. */
export interface TakePagesArgs {
  rows: PageRow[];
}

/** The arguments of `read_outline`. */
export interface ReadOutlineArgs {
}

/** The arguments of `draw_page`. */
export interface DrawPageArgs {
  page: Page;
  width: number;
}

/** The arguments of `draw_next`. */
export interface DrawNextArgs {
}

/** The arguments of `side_by_side`. */
export interface SideBySideArgs {
}

/** The arguments of `editor_view`. */
export interface EditorViewArgs {
}

/** The arguments of `preview_view`. */
export interface PreviewViewArgs {
}

/** The arguments of `problems`. */
export interface ProblemsArgs {
}

/** The arguments of `outline`. */
export interface OutlineArgs {
}

/** The arguments of `zoom_in`. */
export interface ZoomInArgs {
}

/** The arguments of `zoom_out`. */
export interface ZoomOutArgs {
}

/** The arguments of `fit_width`. */
export interface FitWidthArgs {
  width: number;
}

/** The arguments of `turn_to`. */
export interface TurnToArgs {
  page: Page;
}

/** The arguments of `go_to_problem`. */
export interface GoToProblemArgs {
  problem: Problem;
}

/** The arguments of `go_to_heading`. */
export interface GoToHeadingArgs {
  heading: Heading;
}

/** The arguments of `go_to`. */
export interface GoToArgs {
  line: number;
  column: number;
}

/** The arguments of `close_document`. */
export interface CloseDocumentArgs {
  d: Document;
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

/** Problem — `tl.so.tilo.typst-editor.Problem`. */
export interface Problem extends tilo.Resource {
  yt5nyujj: Severity;  // severity
  i3zcji5u: string;  // message
  od6pdrjz: number;  // line
  xrgk5gr2: number;  // column
  f7mcrdxf: number;  // end_line
  "7j5cjuet": number;  // end_column
  u6wlx35y: string[];  // hints
}

/** The actions `Problem` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PROBLEM_ACTIONS = [] as const;

/** The `tl.so.tilo.typst-editor.Problem` kind at `path`. */
export function problem(ws: tilo.Workspace, path: string): Problem {
  return resourceProxy(ws, path, SURFACE, PROBLEM_ACTIONS) as unknown as Problem;
}

/** Page — `tl.so.tilo.typst-editor.Page`. */
export interface Page extends tilo.Resource {
  cgrnobye: number;  // width
  r4wxummm: number;  // height
  "6r2kgenm": string;  // look
  "7ry6v2yt": string;  // picture
  ycgenfy7: number;  // picture_width
  wfjgjbj4: string;  // thumb
  yusfx3a6: number;  // thumb_width
}

/** The actions `Page` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PAGE_ACTIONS = [] as const;

/** The `tl.so.tilo.typst-editor.Page` kind at `path`. */
export function page(ws: tilo.Workspace, path: string): Page {
  return resourceProxy(ws, path, SURFACE, PAGE_ACTIONS) as unknown as Page;
}

/** Heading — `tl.so.tilo.typst-editor.Heading`. */
export interface Heading extends tilo.Resource {
  ougql7d2: string;  // title
  ucauvawo: number;  // line
  t2356zsr: number;  // level
}

/** The actions `Heading` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const HEADING_ACTIONS = [] as const;

/** The `tl.so.tilo.typst-editor.Heading` kind at `path`. */
export function heading(ws: tilo.Workspace, path: string): Heading {
  return resourceProxy(ws, path, SURFACE, HEADING_ACTIONS) as unknown as Heading;
}

/** Document — `tl.so.tilo.typst-editor.Document`. */
export interface Document extends tilo.Resource {
  ojyy5fog: string;  // name
  wigkyjqk: string;  // source
  zlwvg46c: string;  // typeset
  s3mabp2s: boolean;  // typesets
  d7qirnkw: number;  // took_ms
  hk3xko3g: string;  // trouble
  rrdheos7: string;  // written
  bxbra2li: Array<tilo.Entry<Problem>>;  // problems
  jsru5ywo: Array<tilo.Entry<Page>>;  // pages
  cltpfy4y: Array<tilo.Entry<Heading>>;  // headings
  xn4pkllr: string;  // pdf
  e3bwnsci: string;  // pdf_name
  akp7jws5: number;  // shown_width
  sfn7naaa: string;  // showing
  g6gxruuq: string;  // reveal
  xad4v4m3: number;  // reveals
  fijcb57x: Page | null;  // reading_page
  tbdqtt5g: boolean;  // stale
  /** Typesets the document, as the window and the background do. */
  typeset(): Promise<void>;
  /** Runs `typeset` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  typesetMany(items: TypesetArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Typesets the document when its source moved past the last typeset. */
  typeset_if_stale(): Promise<void>;
  /** Runs `typeset_if_stale` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  typeset_if_staleMany(items: TypesetIfStaleArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Lays the problems a typeset reported over the ones the document held, blocking ones first. */
  take_problems(args: {
    rows: ProblemRow[];
  }): Promise<void>;
  /** Runs `take_problems` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  take_problemsMany(items: TakeProblemsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Lays the typeset pages over the ones the document held: a page that draws what it drew keeps its picture, and one that draws something else is drawn again. */
  take_pages(args: {
    rows: PageRow[];
  }): Promise<void>;
  /** Runs `take_pages` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  take_pagesMany(items: TakePagesArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reads the document's headings from its source's syntax. */
  read_outline(): Promise<void>;
  /** Runs `read_outline` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  read_outlineMany(items: ReadOutlineArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Draws one page at `width` pixels wide, and its thumbnail. */
  draw_page(args: {
    page: Page;
    width: number;
  }): Promise<void>;
  /** Runs `draw_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  draw_pageMany(items: DrawPageArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Draws the next page the window's width asks for, one a second: a long document fills in page by page, without anyone waiting for it. */
  draw_next(): Promise<void>;
  /** Runs `draw_next` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  draw_nextMany(items: DrawNextArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows the source beside the page. */
  side_by_side(): Promise<void>;
  /** Runs `side_by_side` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  side_by_sideMany(items: SideBySideArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows the source alone. */
  editor_view(): Promise<void>;
  /** Runs `editor_view` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  editor_viewMany(items: EditorViewArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows the page alone. */
  preview_view(): Promise<void>;
  /** Runs `preview_view` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  preview_viewMany(items: PreviewViewArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows what the typesetter found, or goes back to the page. */
  problems(): Promise<void>;
  /** Runs `problems` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  problemsMany(items: ProblemsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows the document's headings, or goes back to the page. */
  outline(): Promise<void>;
  /** Runs `outline` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  outlineMany(items: OutlineArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Draws the pages a step wider. */
  zoom_in(): Promise<void>;
  /** Runs `zoom_in` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  zoom_inMany(items: ZoomInArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Draws the pages a step narrower. */
  zoom_out(): Promise<void>;
  /** Runs `zoom_out` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  zoom_outMany(items: ZoomOutArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Draws the pages as wide as the room they are shown in. */
  fit_width(args: {
    width: number;
  }): Promise<void>;
  /** Runs `fit_width` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  fit_widthMany(items: FitWidthArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Turns to a page, so the person comes back to it. */
  turn_to(args: {
    page: Page;
  }): Promise<void>;
  /** Runs `turn_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  turn_toMany(items: TurnToArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Goes to the place a problem names. */
  go_to_problem(args: {
    problem: Problem;
  }): Promise<void>;
  /** Runs `go_to_problem` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  go_to_problemMany(items: GoToProblemArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Goes to the line a heading stands on. */
  go_to_heading(args: {
    heading: Heading;
  }): Promise<void>;
  /** Runs `go_to_heading` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  go_to_headingMany(items: GoToHeadingArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts the caret at `line`:`column` and shows it, counting each ask so the same place shows again. */
  go_to(args: {
    line: number;
    column: number;
  }): Promise<void>;
  /** Runs `go_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  go_toMany(items: GoToArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The document as the file it is saved as, marked as written. */
  save_file(): Promise<unknown>;
  /** The PDF the last typeset wrote, as the file a person saves. */
  export_pdf(): Promise<unknown>;
  /** Copies the words a person selected on a page. */
  copy_selection(args: {
    page_at: number;
    from?: number[];
    to?: number[];
  }): Promise<void>;
  /** Copies all of a page's words. */
  copy_page(args: {
    page_at: number;
  }): Promise<void>;
  /** All of a page's words, as the typesetter read them. */
  page_text(args: {
    page_at: number;
  }): Promise<string>;
  /** The words of a page inside the box from `start` to `end`. */
  selection_text(args: {
    page_at: number;
    start: number[];
    end: number[];
  }): Promise<string>;
  /** The placed glyphs of one page, as the typesetter read them. */
  page_words(args: {
    page_at: number;
  }): Promise<CellRow[]>;
}

/** The actions `Document` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DOCUMENT_ACTIONS = ["typeset", "typesetMany", "typeset_if_stale", "typeset_if_staleMany", "take_problems", "take_problemsMany", "take_pages", "take_pagesMany", "read_outline", "read_outlineMany", "draw_page", "draw_pageMany", "draw_next", "draw_nextMany", "side_by_side", "side_by_sideMany", "editor_view", "editor_viewMany", "preview_view", "preview_viewMany", "problems", "problemsMany", "outline", "outlineMany", "zoom_in", "zoom_inMany", "zoom_out", "zoom_outMany", "fit_width", "fit_widthMany", "turn_to", "turn_toMany", "go_to_problem", "go_to_problemMany", "go_to_heading", "go_to_headingMany", "go_to", "go_toMany", "save_file", "export_pdf", "copy_selection", "copy_page", "page_text", "selection_text", "page_words"] as const;

/** The `tl.so.tilo.typst-editor.Document` kind at `path`. */
export function typstEditorDocument(ws: tilo.Workspace, path: string): Document {
  return resourceProxy(ws, path, SURFACE, DOCUMENT_ACTIONS) as unknown as Document;
}

/** Library — `tl.so.tilo.typst-editor.Library`. */
export interface Library extends tilo.Resource {
  jdegntgs: Array<tilo.Entry<Document>>;  // documents
  /** A Typst file opens in the editor: Files, the person's own file system and the system's search hand it over as the file it is, and it opens on the document those words are. */
  open_typ(args: {
    file: unknown;
  }): Promise<Document>;
  /** A new, empty document. */
  new_document(args?: {
    name?: string;
    source?: string;
  }): Promise<Document>;
  /** A document leaves the workspace. */
  close_document(args: {
    d: Document;
  }): Promise<void>;
  /** Runs `close_document` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  close_documentMany(items: CloseDocumentArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Library` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LIBRARY_ACTIONS = ["open_typ", "new_document", "close_document", "close_documentMany"] as const;

/** The `tl.so.tilo.typst-editor.Library` kind at `path`. */
export function library(ws: tilo.Workspace, path: string): Library {
  return resourceProxy(ws, path, SURFACE, LIBRARY_ACTIONS) as unknown as Library;
}

/** CompileAsk — `tl.so.tilo.typst-editor.CompileAsk`. */
export interface CompileAsk extends tilo.Resource {
  d2jzgpyx: string;  // source
}

/** The actions `CompileAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const COMPILEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.typst-editor.CompileAsk` kind at `path`. */
export function compileAsk(ws: tilo.Workspace, path: string): CompileAsk {
  return resourceProxy(ws, path, SURFACE, COMPILEASK_ACTIONS) as unknown as CompileAsk;
}

/** PageAsk — `tl.so.tilo.typst-editor.PageAsk`. */
export interface PageAsk extends tilo.Resource {
  a66w2zy6: string;  // source
  "6jynr6xk": number;  // page
  m7iybhvb: number;  // width
}

/** The actions `PageAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PAGEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.typst-editor.PageAsk` kind at `path`. */
export function pageAsk(ws: tilo.Workspace, path: string): PageAsk {
  return resourceProxy(ws, path, SURFACE, PAGEASK_ACTIONS) as unknown as PageAsk;
}

/** TextAsk — `tl.so.tilo.typst-editor.TextAsk`. */
export interface TextAsk extends tilo.Resource {
  rydvb6fi: string;  // source
  rqodn5jw: number;  // page
}

/** The actions `TextAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TEXTASK_ACTIONS = [] as const;

/** The `tl.so.tilo.typst-editor.TextAsk` kind at `path`. */
export function textAsk(ws: tilo.Workspace, path: string): TextAsk {
  return resourceProxy(ws, path, SURFACE, TEXTASK_ACTIONS) as unknown as TextAsk;
}

/** OutlineAsk — `tl.so.tilo.typst-editor.OutlineAsk`. */
export interface OutlineAsk extends tilo.Resource {
  wevbiksf: string;  // source
}

/** The actions `OutlineAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OUTLINEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.typst-editor.OutlineAsk` kind at `path`. */
export function outlineAsk(ws: tilo.Workspace, path: string): OutlineAsk {
  return resourceProxy(ws, path, SURFACE, OUTLINEASK_ACTIONS) as unknown as OutlineAsk;
}

/** PageRow — `tl.so.tilo.typst-editor.PageRow`. */
export interface PageRow extends tilo.Resource {
  mtlxa3ps: number;  // width
  ay4kqc7o: number;  // height
  ehra7vlr: string;  // look
}

/** The actions `PageRow` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PAGEROW_ACTIONS = [] as const;

/** The `tl.so.tilo.typst-editor.PageRow` kind at `path`. */
export function pageRow(ws: tilo.Workspace, path: string): PageRow {
  return resourceProxy(ws, path, SURFACE, PAGEROW_ACTIONS) as unknown as PageRow;
}

/** ProblemRow — `tl.so.tilo.typst-editor.ProblemRow`. */
export interface ProblemRow extends tilo.Resource {
  "7vzeic6n": string;  // kind
  "55dyzse3": string;  // message
  bzomsyem: number | null;  // line
  vwara56h: number | null;  // column
  lewaebu2: number | null;  // end_line
  unj2zbun: number | null;  // end_column
  wakty2b2: string[];  // hints
}

/** The actions `ProblemRow` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PROBLEMROW_ACTIONS = [] as const;

/** The `tl.so.tilo.typst-editor.ProblemRow` kind at `path`. */
export function problemRow(ws: tilo.Workspace, path: string): ProblemRow {
  return resourceProxy(ws, path, SURFACE, PROBLEMROW_ACTIONS) as unknown as ProblemRow;
}

/** CompiledRow — `tl.so.tilo.typst-editor.CompiledRow`. */
export interface CompiledRow extends tilo.Resource {
  ffvtdeuv: boolean;  // ok
  t5b6wta3: number;  // elapsed_ms
  siiy7bea: string | null;  // pdf_url
  htehc63o: string | null;  // pdf_name
  "63pe7jqy": number | null;  // pdf_size
  uryvrrzv: string | null;  // pdf_trouble
}

/** The actions `CompiledRow` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const COMPILEDROW_ACTIONS = [] as const;

/** The `tl.so.tilo.typst-editor.CompiledRow` kind at `path`. */
export function compiledRow(ws: tilo.Workspace, path: string): CompiledRow {
  return resourceProxy(ws, path, SURFACE, COMPILEDROW_ACTIONS) as unknown as CompiledRow;
}

/** HeadingRow — `tl.so.tilo.typst-editor.HeadingRow`. */
export interface HeadingRow extends tilo.Resource {
  "3olgjoh6": number;  // line
  lcdznshm: number;  // level
  pq3mu5rf: string;  // title
}

/** The actions `HeadingRow` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const HEADINGROW_ACTIONS = [] as const;

/** The `tl.so.tilo.typst-editor.HeadingRow` kind at `path`. */
export function headingRow(ws: tilo.Workspace, path: string): HeadingRow {
  return resourceProxy(ws, path, SURFACE, HEADINGROW_ACTIONS) as unknown as HeadingRow;
}

/** CellRow — `tl.so.tilo.typst-editor.CellRow`. */
export interface CellRow extends tilo.Resource {
  dsvn4b2o: string;  // text
  erwowgdw: number;  // x0
  txaxsk54: number;  // x1
  vobo3z4e: number;  // top
  w6ydh2nv: number;  // bottom
  "73rf4vlf": number;  // line
}

/** The actions `CellRow` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CELLROW_ACTIONS = [] as const;

/** The `tl.so.tilo.typst-editor.CellRow` kind at `path`. */
export function cellRow(ws: tilo.Workspace, path: string): CellRow {
  return resourceProxy(ws, path, SURFACE, CELLROW_ACTIONS) as unknown as CellRow;
}

/** Typst — `tl.so.tilo.typst-editor`. */
export interface TypstEditor extends tilo.Resource {
}

/** The actions `TypstEditor` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TYPSTEDITOR_ACTIONS = [] as const;

/** The `tl.so.tilo.typst-editor` kind at `path`. */
export function typstEditor(ws: tilo.Workspace, path: string): TypstEditor {
  return resourceProxy(ws, path, SURFACE, TYPSTEDITOR_ACTIONS) as unknown as TypstEditor;
}

