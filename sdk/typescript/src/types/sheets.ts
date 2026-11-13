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
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** The version of the surface these types were made from: a workspace whose app
 * publishes another refuses their actions rather than run what they no longer
 * describe. */
export const SURFACE = "so.tilo.sheets@9ade0694b6a2e150";

export interface Area {
  top: string;
  left: string;
  bottom: string;
  right: string;
}

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

/** One of "column", "line". */
export type ChartKind = "column" | "line";

/** One of bool, int, str: an object whose case the app tags it with. */
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
  author: string;
  date: string;
  text: string;
  body: ClipBlock[];
  resolved: boolean;
  replies: Reply[];
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

/** One of bool, int, str: an object whose case the app tags it with. */
export type MarkValue = Record<string, unknown>;

export interface Reply {
  author: string;
  date: string;
  text: string;
  body: ClipBlock[];
}

export interface Sheet {
  name: string;
  source: string | null;
  color: number | null;
  cells: unknown;
  charts: Chart[];
  hidden: boolean;
}

/** One of "Language", "H24", "H12". */
export type TimeStyle = "Language" | "H24" | "H12";

/** Workbook — `sheets.workbook`. */
export interface Workbook extends tilo.Resource {
  sheets: Sheet[];
  comments: Comment[];
  settings: DocSettings;
  title: string | null;
  newSheet(args?: {
    name?: string;
    after?: string;
    rows?: unknown[][];
    columns?: string[];
    records?: unknown[];
    formats?: unknown;
    workbook?: string;
  }): Promise<unknown | tilo.Run>;
  setTitle(args: {
    title: string;
  }): Promise<tilo.Run | void>;
  setRecords(args: {
    sheet: string;
    records: unknown[];
    row?: number;
    columns?: string[];
    key?: string;
  }): Promise<unknown | tilo.Run>;
  upsertItem(args: {
    sheet: string;
    match: Record<string, unknown>;
    value: Record<string, unknown>;
  }): Promise<unknown | tilo.Run>;
  duplicateSheet(args: {
    sheet: string;
  }): Promise<tilo.Run | void>;
  deleteSheet(args: {
    sheet: string;
  }): Promise<tilo.Run | void>;
  setCells(args: {
    sheet: string;
    start: string;
    rows: unknown[][];
  }): Promise<unknown | tilo.Run>;
  writeCells(args: {
    sheet: string;
    cells: CellWrite[];
  }): Promise<unknown | tilo.Run>;
  formatCells(args: {
    sheet: string;
    range: string;
    format?: unknown;
    bold?: boolean;
  }): Promise<unknown | tilo.Run>;
  sortRows(args: {
    sheet: string;
    range: string;
    by: string;
    descending?: boolean;
  }): Promise<unknown | tilo.Run>;
  insertRows(args: {
    sheet: string;
    at: number;
    count?: number;
  }): Promise<tilo.Run | void>;
  deleteRows(args: {
    sheet: string;
    rows: number[];
  }): Promise<unknown | tilo.Run>;
  insertColumns(args: {
    sheet: string;
    at: string;
    count?: number;
  }): Promise<tilo.Run | void>;
  deleteColumns(args: {
    sheet: string;
    columns: string[];
  }): Promise<tilo.Run | void>;
  importSheets(args: {
    sheets: ImportedSheet[];
    source?: string;
    title?: string;
  }): Promise<tilo.Run | void>;
  addChart(args: {
    sheet: string;
    range: string;
    kind: string;
    title: string;
  }): Promise<tilo.Run | void>;
  editChart(args: {
    sheet: string;
    chart: string;
    title: string;
    kind?: unknown;
    range?: unknown;
  }): Promise<tilo.Run | void>;
  deleteChart(args: {
    sheet: string;
    chart: string;
  }): Promise<tilo.Run | void>;
  renameSheet(args: {
    sheet: string;
    name: string;
  }): Promise<tilo.Run | void>;
  comment(args: {
    sheet: string;
    range: string;
    text: string;
  }): Promise<tilo.Run | void>;
  reply(args: {
    comment: string;
    text: string;
  }): Promise<tilo.Run | void>;
  editComment(args: {
    comment: string;
    body: string;
  }): Promise<tilo.Run | void>;
  deleteComment(args: {
    comment: string;
  }): Promise<tilo.Run | void>;
  exportXlsxContent(): Promise<void>;
}

/** The `sheets.workbook` kind at `path`. */
export function workbook(ws: tilo.Workspace, path: string): Workbook {
  return resourceProxy(ws, path, SURFACE) as unknown as Workbook;
}

