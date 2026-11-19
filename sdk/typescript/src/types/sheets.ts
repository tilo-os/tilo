/**
 * Types for Sheets (`so.tilo.sheets`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Workbook` is a `Resource` of the app's `sheets.workbook`, made by its factory with the
 * resource's path (`sheets/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.sheets@f729b94191b1093c";

export interface Area {
  top: string;
  left: string;
  bottom: string;
  right: string;
}

/** One of member, named: an object whose case the app tags it with. */
export type Author = Record<string, unknown>;

export interface CellWrite {
  cell: string;
  input: string | null;
  format: unknown | null;
  bold: boolean | null;
}

export interface Chart {
  title: string;
  kind: ChartKind;
  area: Area;
}

/** One of "Column", "Line". */
export type ChartKind = "Column" | "Line";

/** One of Bool, Int, Str: an object whose case the app tags it with. */
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
  anchor: CommentCells;
  author: Author;
  date: string;
  text: string;
  body: ClipBlock[];
  resolved: boolean;
  replies: Array<tilo.Entry<Reply>>;
}

export interface CommentCells {
  sheet: string;
  area: Area;
}

/** One of "USD", "EUR", "GBP", "JPY", "CHF", "CAD", "AUD", "CNY", "INR". */
export type Currency = "USD" | "EUR" | "GBP" | "JPY" | "CHF" | "CAD" | "AUD" | "CNY" | "INR";

/** One of "Language", "ISO", "US", "EU", "Long". */
export type DateStyle = "Language" | "ISO" | "US" | "EU" | "Long";

export interface DocSettings {
  currency: Currency | null;
  date_format: DateStyle | null;
  time_format: TimeStyle | null;
}

export interface ImportedSheet {
  name: string;
  cells: CellWrite[];
  hidden: boolean;
}

export interface MarkSpan {
  start: number;
  end: number;
  name: string;
  value: MarkValue;
}

/** One of Bool, Int, Str: an object whose case the app tags it with. */
export type MarkValue = Record<string, unknown>;

export interface Reply {
  author: Author;
  date: string;
  text: string;
  body: ClipBlock[];
}

export interface Sheet {
  name: string;
  source: string | null;
  color: number | null;
  cells: unknown;
  charts: Array<tilo.Entry<Chart>>;
  hidden: boolean;
  cut: string[];
}

export interface SheetSpec {
  name: string | null;
  after: string | null;
  rows: unknown[][] | null;
  columns: string[] | null;
  records: unknown[] | null;
  formats: Record<string, string> | null;
  workbook: string | null;
}

/** One of "Language", "H24", "H12". */
export type TimeStyle = "Language" | "H24" | "H12";

/** The arguments of `set-title`. */
export interface SetTitleArgs {
  title: string;
}

/** The arguments of `duplicate-sheet`. */
export interface DuplicateSheetArgs {
  sheet: string;
}

/** The arguments of `delete-sheet`. */
export interface DeleteSheetArgs {
  sheet: string;
}

/** The arguments of `insert-rows`. */
export interface InsertRowsArgs {
  sheet: string;
  at: number;
  count?: number;
}

/** The arguments of `insert-columns`. */
export interface InsertColumnsArgs {
  sheet: string;
  at: string;
  count?: number;
}

/** The arguments of `delete-columns`. */
export interface DeleteColumnsArgs {
  sheet: string;
  columns: string[];
}

/** The arguments of `import-sheets`. */
export interface ImportSheetsArgs {
  sheets: ImportedSheet[];
  source?: string;
  title?: string;
}

/** The arguments of `add-chart`. */
export interface AddChartArgs {
  sheet: string;
  range: string;
  kind: string;
  title: string;
}

/** The arguments of `edit-chart`. */
export interface EditChartArgs {
  sheet: string;
  chart: string;
  title: string;
  kind?: unknown;
  range?: unknown;
}

/** The arguments of `delete-chart`. */
export interface DeleteChartArgs {
  sheet: string;
  chart: string;
}

/** The arguments of `rename-sheet`. */
export interface RenameSheetArgs {
  sheet: string;
  name: string;
}

/** The arguments of `comment`. */
export interface CommentArgs {
  sheet: string;
  range: string;
  text: string;
}

/** The arguments of `reply`. */
export interface ReplyArgs {
  comment: string;
  text: string;
}

/** The arguments of `edit-comment`. */
export interface EditCommentArgs {
  comment: string;
  body: string;
}

/** The arguments of `delete-comment`. */
export interface DeleteCommentArgs {
  comment: string;
}

/** Workbook — `sheets.workbook`. */
export interface Workbook extends tilo.Resource {
  sheets: Array<tilo.Entry<Sheet>>;
  comments: Array<tilo.Entry<Comment>>;
  settings: DocSettings;
  title: string | null;
  newSheet(args?: {
    name?: string;
    after?: string;
    rows?: unknown[][];
    columns?: string[];
    records?: unknown[];
    formats?: Record<string, string>;
    workbook?: string;
  }): Promise<unknown>;
  setTitle(args: {
    title: string;
  }): Promise<void>;
  /** Runs `set-title` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setTitleMany(items: SetTitleArgs[]): Promise<tilo.ActionAnswer[]>;
  setRecords(args: {
    sheet: string;
    records: unknown[];
    row?: number;
    columns?: string[];
    key?: string;
  }): Promise<unknown>;
  upsertItem(args: {
    sheet: string;
    match: Record<string, unknown>;
    value: Record<string, unknown>;
  }): Promise<unknown>;
  duplicateSheet(args: {
    sheet: string;
  }): Promise<void>;
  /** Runs `duplicate-sheet` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  duplicateSheetMany(items: DuplicateSheetArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteSheet(args: {
    sheet: string;
  }): Promise<void>;
  /** Runs `delete-sheet` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteSheetMany(items: DeleteSheetArgs[]): Promise<tilo.ActionAnswer[]>;
  setCells(args: {
    sheet: string;
    start: string;
    rows: unknown[][];
    seen?: string;
  }): Promise<unknown>;
  writeCells(args: {
    sheet: string;
    cells: CellWrite[];
    seen?: string;
  }): Promise<unknown>;
  formatCells(args: {
    sheet: string;
    range: string;
    format?: unknown;
    bold?: boolean;
  }): Promise<unknown>;
  sortRows(args: {
    sheet: string;
    range: string;
    by: string;
    descending?: boolean;
  }): Promise<unknown>;
  insertRows(args: {
    sheet: string;
    at: number;
    count?: number;
  }): Promise<void>;
  /** Runs `insert-rows` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  insertRowsMany(items: InsertRowsArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteRows(args: {
    sheet: string;
    rows: number[];
  }): Promise<unknown>;
  insertColumns(args: {
    sheet: string;
    at: string;
    count?: number;
  }): Promise<void>;
  /** Runs `insert-columns` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  insertColumnsMany(items: InsertColumnsArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteColumns(args: {
    sheet: string;
    columns: string[];
  }): Promise<void>;
  /** Runs `delete-columns` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteColumnsMany(items: DeleteColumnsArgs[]): Promise<tilo.ActionAnswer[]>;
  importSheets(args: {
    sheets: ImportedSheet[];
    source?: string;
    title?: string;
  }): Promise<void>;
  /** Runs `import-sheets` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  importSheetsMany(items: ImportSheetsArgs[]): Promise<tilo.ActionAnswer[]>;
  addChart(args: {
    sheet: string;
    range: string;
    kind: string;
    title: string;
  }): Promise<void>;
  /** Runs `add-chart` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  addChartMany(items: AddChartArgs[]): Promise<tilo.ActionAnswer[]>;
  editChart(args: {
    sheet: string;
    chart: string;
    title: string;
    kind?: unknown;
    range?: unknown;
  }): Promise<void>;
  /** Runs `edit-chart` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  editChartMany(items: EditChartArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteChart(args: {
    sheet: string;
    chart: string;
  }): Promise<void>;
  /** Runs `delete-chart` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteChartMany(items: DeleteChartArgs[]): Promise<tilo.ActionAnswer[]>;
  renameSheet(args: {
    sheet: string;
    name: string;
  }): Promise<void>;
  /** Runs `rename-sheet` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  renameSheetMany(items: RenameSheetArgs[]): Promise<tilo.ActionAnswer[]>;
  comment(args: {
    sheet: string;
    range: string;
    text: string;
  }): Promise<void>;
  /** Runs `comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  commentMany(items: CommentArgs[]): Promise<tilo.ActionAnswer[]>;
  reply(args: {
    comment: string;
    text: string;
  }): Promise<void>;
  /** Runs `reply` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  replyMany(items: ReplyArgs[]): Promise<tilo.ActionAnswer[]>;
  editComment(args: {
    comment: string;
    body: string;
  }): Promise<void>;
  /** Runs `edit-comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  editCommentMany(items: EditCommentArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteComment(args: {
    comment: string;
  }): Promise<void>;
  /** Runs `delete-comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteCommentMany(items: DeleteCommentArgs[]): Promise<tilo.ActionAnswer[]>;
  exportXlsxContent(): Promise<void>;
  exportCsvContent(args?: {
    sheet?: string;
  }): Promise<void>;
  readFile(args: {
    file: string;
    sheet?: string;
    from?: number;
  }): Promise<void>;
  readContent(args: {
    content: string;
    name: string;
    sheet?: string;
    from?: number;
  }): Promise<void>;
  readCells(args: {
    sheet: string;
    range?: string;
    from?: number;
    shown?: boolean;
  }): Promise<void>;
}

/** The actions `Workbook` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WORKBOOK_ACTIONS = ["newSheet", "setTitle", "setTitleMany", "setRecords", "upsertItem", "duplicateSheet", "duplicateSheetMany", "deleteSheet", "deleteSheetMany", "setCells", "writeCells", "formatCells", "sortRows", "insertRows", "insertRowsMany", "deleteRows", "insertColumns", "insertColumnsMany", "deleteColumns", "deleteColumnsMany", "importSheets", "importSheetsMany", "addChart", "addChartMany", "editChart", "editChartMany", "deleteChart", "deleteChartMany", "renameSheet", "renameSheetMany", "comment", "commentMany", "reply", "replyMany", "editComment", "editCommentMany", "deleteComment", "deleteCommentMany", "exportXlsxContent", "exportCsvContent", "readFile", "readContent", "readCells"] as const;

/** The `sheets.workbook` kind at `path`. */
export function workbook(ws: tilo.Workspace, path: string): Workbook {
  return resourceProxy(ws, path, SURFACE, WORKBOOK_ACTIONS) as unknown as Workbook;
}

