/**
 * Types for Sheets (`so.tilo.sheets`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Workbook` is a `Resource` of the app's `tl.so.tilo.sheets.Workbook`, made by its factory with the
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
export const SURFACE = "so.tilo.sheets@772f8bff971fef35";

/** What a chart draws: columns side by side, or a line over its points. One of "column", "line". */
export type ChartKind = "column" | "line";

/** The currency a bare money format shows in. One of "usd", "eur", "gbp", "jpy", "chf", "cad", "aud", "cny", "inr". */
export type Currency = "usd" | "eur" | "gbp" | "jpy" | "chf" | "cad" | "aud" | "cny" | "inr";

/** How a cell writes a date: the language's own, ISO, a US or European order, or spelled out. One of "language", "iso", "us", "eu", "long". */
export type DateStyle = "language" | "iso" | "us" | "eu" | "long";

/** How a cell writes a clock time: the language's own, 24-hour or 12-hour. One of "language", "h24", "h12". */
export type TimeStyle = "language" | "h24" | "h12";

/** The arguments of `set_title`. */
export interface SetTitleArgs {
  title: string;
}

/** The arguments of `rename_sheet`. */
export interface RenameSheetArgs {
  s: Sheet;
  name: string;
}

/** The arguments of `delete_sheet`. */
export interface DeleteSheetArgs {
  s: Sheet;
}

/** The arguments of `place_sheet`. */
export interface PlaceSheetArgs {
  s: Sheet;
  before: Sheet;
}

/** The arguments of `move_sheet`. */
export interface MoveSheetArgs {
  s: Sheet;
  at: number;
}

/** The arguments of `write_cells`. */
export interface WriteCellsArgs {
  sheet: string;
  refs: string[];
  inputs: string[];
  seen?: string;
}

/** The arguments of `set_cells`. */
export interface SetCellsArgs {
  s: Sheet;
  start: string;
  rows: string[][];
  seen?: string;
}

/** The arguments of `paste_cells`. */
export interface PasteCellsArgs {
  s: Sheet;
  at: string;
}

/** The arguments of `clear_cells`. */
export interface ClearCellsArgs {
  s: Sheet;
  area: string;
}

/** The arguments of `format_cells`. */
export interface FormatCellsArgs {
  s: Sheet;
  range: string;
  format?: string;
  bold?: boolean;
}

/** The arguments of `toggle_bold`. */
export interface ToggleBoldArgs {
  s: Sheet;
  range: string;
}

/** The arguments of `autosum`. */
export interface AutosumArgs {
  s: Sheet;
  at: string;
}

/** The arguments of `sum_selection`. */
export interface SumSelectionArgs {
  s: Sheet;
  range: string;
}

/** The arguments of `insert_rows`. */
export interface InsertRowsArgs {
  s: Sheet;
  at: number;
  count?: number;
}

/** The arguments of `delete_rows`. */
export interface DeleteRowsArgs {
  s: Sheet;
  rows: number[];
}

/** The arguments of `insert_columns`. */
export interface InsertColumnsArgs {
  s: Sheet;
  at: string;
  count?: number;
}

/** The arguments of `delete_columns`. */
export interface DeleteColumnsArgs {
  s: Sheet;
  columns: string[];
}

/** The arguments of `sort_rows`. */
export interface SortRowsArgs {
  s: Sheet;
  range: string;
  by: string;
  descending?: boolean;
}

/** The arguments of `fill`. */
export interface FillArgs {
  s: Sheet;
  pair: string[];
}

/** The arguments of `apply_edit`. */
export interface ApplyEditArgs {
  change: string;
}

/** The arguments of `delete_comment`. */
export interface DeleteCommentArgs {
  c: Comment;
}

/** The arguments of `set_settings`. */
export interface SetSettingsArgs {
  currency?: Currency;
  date_style?: DateStyle;
  time_style?: TimeStyle;
}

/** The arguments of `import_table`. */
export interface ImportTableArgs {
  file: unknown;
}

/** The arguments of `edit_cells`. */
export interface EditCellsArgs {
  s: Sheet;
  pairs: string[];
}

/** The arguments of `set_sheet_hidden`. */
export interface SetSheetHiddenArgs {
  hidden: boolean;
}

/** The arguments of `set_sheet_color`. */
export interface SetSheetColorArgs {
  color: number;
}

/** The arguments of `clear_sheet_color`. */
export interface ClearSheetColorArgs {
}

/** The arguments of `delete_chart`. */
export interface DeleteChartArgs {
  c: Chart;
}

/** The arguments of `resize_column`. */
export interface ResizeColumnArgs {
  pair: string[];
}

/** The arguments of `resize_row`. */
export interface ResizeRowArgs {
  pair: string[];
}

/** The arguments of `edit_chart`. */
export interface EditChartArgs {
  title?: string;
  kind?: ChartKind;
  range?: string;
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

/** The arguments of `resolve`. */
export interface ResolveArgs {
  resolved?: boolean;
}

/** The arguments of `edit_comment`. */
export interface EditCommentArgs {
  text?: string;
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

/** The arguments of `edit_reply`. */
export interface EditReplyArgs {
  text?: string;
}

/** Workbook — `tl.so.tilo.sheets.Workbook`. */
export interface Workbook extends tilo.Resource {
  meovofcz: string;  // title
  pkbcb37f: Array<tilo.Entry<Sheet>>;  // sheets
  xwh3fsa2: Array<tilo.Entry<Comment>>;  // comments
  r7ipin2u: Currency;  // currency
  "33jfkdmv": DateStyle;  // date_style
  wvlwcqju: TimeStyle;  // time_style
  qetlxegv: number;  // structure
  /** What the cells of `range` of the sheet named `sheet` show, row by row from its first cell to its last, or their input as typed when `formulas`: what a person reads in the grid, for an agent that never opened the window. */
  read_cells(args: {
    sheet: string;
    range?: string;
    formulas?: boolean;
  }): Promise<string[]>;
  /** Adds a sheet at the end, named `name` or the next free `SheetN`. */
  new_sheet(args?: {
    name?: string;
  }): Promise<Sheet>;
  /** Adds a sheet right after `after`, so a new tab stands where the person is. */
  new_sheet_after(args: {
    after: Sheet;
    name?: string;
  }): Promise<Sheet>;
  /** Names the workbook, so it is found and listed by that name. */
  set_title(args: {
    title: string;
  }): Promise<void>;
  /** Runs `set_title` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_titleMany(items: SetTitleArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Renames a sheet; every formula that reads it follows it to its new name. */
  rename_sheet(args: {
    s: Sheet;
    name: string;
  }): Promise<void>;
  /** Runs `rename_sheet` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rename_sheetMany(items: RenameSheetArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Copies a sheet and everything on it, under a name of its own, right after it. */
  duplicate_sheet(args: {
    s: Sheet;
  }): Promise<Sheet>;
  /** Deletes a sheet and what was said about it. */
  delete_sheet(args: {
    s: Sheet;
  }): Promise<void>;
  /** Runs `delete_sheet` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_sheetMany(items: DeleteSheetArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts a sheet's tab right before another, so the person reorders the tabs. */
  place_sheet(args: {
    s: Sheet;
    before: Sheet;
  }): Promise<void>;
  /** Runs `place_sheet` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  place_sheetMany(items: PlaceSheetArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves a sheet's tab to `at`, the place among the tabs a person sees that a drag reports. */
  move_sheet(args: {
    s: Sheet;
    at: number;
  }): Promise<void>;
  /** Runs `move_sheet` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_sheetMany(items: MoveSheetArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Writes the inputs `inputs` into the cells `refs`, by position: an empty input clears its cell. A typed entry keeps the format it is read in (a date, money, a percent), unless the cell has one the person set. A `seen` stamp refuses the write when rows, columns or sheets have moved since it was read. */
  write_cells(args: {
    sheet: string;
    refs: string[];
    inputs: string[];
    seen?: string;
  }): Promise<void>;
  /** Runs `write_cells` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  write_cellsMany(items: WriteCellsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets cells from a start cell, row by row: what a table handed to the workbook writes. */
  set_cells(args: {
    s: Sheet;
    start: string;
    rows: string[][];
    seen?: string;
  }): Promise<void>;
  /** Runs `set_cells` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_cellsMany(items: SetCellsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Writes records into a sheet by column header: each record an object keyed by the sheet's headers, so a value a record leaves out leaves its cell instead of shifting the row after it. `row` is the first data row (the row under the headers when left out), `columns` the headers a sheet that has none starts with, and `key` the header that identifies a record's row rather than writing a new one. */
  set_records(args: {
    s: Sheet;
    records: Array<Record<string, string>>;
    row?: number;
    columns?: string[];
    key?: string;
  }): Promise<unknown>;
  /** Writes a record into a sheet by the values that identify its row: `matching` names the headers and values that find the row, and `value` the cells it takes. A row nothing matches is made under the headers. */
  upsert_item(args: {
    s: Sheet;
    matching: Record<string, string>;
    value: Record<string, string>;
  }): Promise<unknown>;
  /** Cuts the cells a person chose: copies them as `copy_cells` does, then clears them. */
  cut_cells(args: {
    s: Sheet;
    area: string;
    text: string;
  }): Promise<void>;
  /** Writes the table on the person's clipboard into the cells from `at`: a line per row and a tab between columns. Nothing on the clipboard writes nothing. */
  paste_cells(args: {
    s: Sheet;
    at: string;
  }): Promise<void>;
  /** Runs `paste_cells` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  paste_cellsMany(items: PasteCellsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Clears the cells `area` names. */
  clear_cells(args: {
    s: Sheet;
    area: string;
  }): Promise<void>;
  /** Runs `clear_cells` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_cellsMany(items: ClearCellsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets the format of the cells `range` (a code such as `currency:EUR`, `percent` or `date:ISO`), and their weight when `bold` is given. */
  format_cells(args: {
    s: Sheet;
    range: string;
    format?: string;
    bold?: boolean;
  }): Promise<void>;
  /** Runs `format_cells` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  format_cellsMany(items: FormatCellsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Makes the cells `range` bold, or regular when any one of them already is. */
  toggle_bold(args: {
    s: Sheet;
    range: string;
  }): Promise<void>;
  /** Runs `toggle_bold` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  toggle_boldMany(items: ToggleBoldArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Writes the sum of the numbers above `at` into it, the run stopping at a blank, a word or an existing total: the AutoSum button. */
  autosum(args: {
    s: Sheet;
    at: string;
  }): Promise<void>;
  /** Runs `autosum` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  autosumMany(items: AutosumArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds up each column of the selection into the row under it. */
  sum_selection(args: {
    s: Sheet;
    range: string;
  }): Promise<void>;
  /** Runs `sum_selection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  sum_selectionMany(items: SumSelectionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Inserts `count` rows above the row `at`, counting from 1. */
  insert_rows(args: {
    s: Sheet;
    at: number;
    count?: number;
  }): Promise<void>;
  /** Runs `insert_rows` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  insert_rowsMany(items: InsertRowsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes the rows `rows`, counting from 1. */
  delete_rows(args: {
    s: Sheet;
    rows: number[];
  }): Promise<void>;
  /** Runs `delete_rows` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_rowsMany(items: DeleteRowsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Inserts `count` columns left of the column `at`, by its letters. */
  insert_columns(args: {
    s: Sheet;
    at: string;
    count?: number;
  }): Promise<void>;
  /** Runs `insert_columns` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  insert_columnsMany(items: InsertColumnsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes the columns `columns`, by their letters. */
  delete_columns(args: {
    s: Sheet;
    columns: string[];
  }): Promise<void>;
  /** Runs `delete_columns` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_columnsMany(items: DeleteColumnsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sorts the rows of `range` by the values under the column `by`, by its letters. */
  sort_rows(args: {
    s: Sheet;
    range: string;
    by: string;
    descending?: boolean;
  }): Promise<void>;
  /** Runs `sort_rows` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  sort_rowsMany(items: SortRowsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Extends the cells `source` over `target`, as dragging the fill handle does: numbers count on, dates follow their step and a pattern of words repeats. The pair carries the cells it copies from and the cells it fills, as the grid reports them. */
  fill(args: {
    s: Sheet;
    pair: string[];
  }): Promise<void>;
  /** Runs `fill` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  fillMany(items: FillArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Applies one structural change to the workbook and keeps every sheet as the engine leaves it: its cells, their formats and the cells shown bold, its name when the change renamed it, and the cells its charts and comments are about. */
  apply_edit(args: {
    change: string;
  }): Promise<void>;
  /** Runs `apply_edit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  apply_editMany(items: ApplyEditArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Says `text` about the cells `range` of a sheet. */
  comment(args: {
    sheet: string;
    range: string;
    text: string;
  }): Promise<Comment>;
  /** Deletes a comment and its replies. Only its author may. */
  delete_comment(args: {
    c: Comment;
  }): Promise<void>;
  /** Runs `delete_comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_commentMany(items: DeleteCommentArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets the workbook's currency, date style and time style: what a cell's bare money, date and time formats are read in. */
  set_settings(args?: {
    currency?: Currency;
    date_style?: DateStyle;
    time_style?: TimeStyle;
  }): Promise<void>;
  /** Runs `set_settings` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_settingsMany(items: SetSettingsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Imports a table, a workbook or a Notion export as new sheets after the ones the workbook holds. */
  import_table(args: {
    file: unknown;
  }): Promise<void>;
  /** Runs `import_table` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  import_tableMany(items: ImportTableArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The workbook as the engine reads it, as text: its sheets, its cells as typed and the currency its bare money format shows in. An agent reads a sheet's content without the window. */
  read_content(): Promise<string>;
  /** The workbook as an Excel file. */
  export_workbook(): Promise<unknown>;
  /** The workbook as an OpenDocument spreadsheet. */
  export_ods(): Promise<unknown>;
  /** One sheet as a table file. */
  export_table(args: {
    s: Sheet;
  }): Promise<unknown>;
  /** Writes the cells a grid edit reports: pairs of reference and input, in order. */
  edit_cells(args: {
    s: Sheet;
    pairs: string[];
  }): Promise<void>;
  /** Runs `edit_cells` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  edit_cellsMany(items: EditCellsArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Workbook` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WORKBOOK_ACTIONS = ["read_cells", "new_sheet", "new_sheet_after", "set_title", "set_titleMany", "rename_sheet", "rename_sheetMany", "duplicate_sheet", "delete_sheet", "delete_sheetMany", "place_sheet", "place_sheetMany", "move_sheet", "move_sheetMany", "write_cells", "write_cellsMany", "set_cells", "set_cellsMany", "set_records", "upsert_item", "cut_cells", "paste_cells", "paste_cellsMany", "clear_cells", "clear_cellsMany", "format_cells", "format_cellsMany", "toggle_bold", "toggle_boldMany", "autosum", "autosumMany", "sum_selection", "sum_selectionMany", "insert_rows", "insert_rowsMany", "delete_rows", "delete_rowsMany", "insert_columns", "insert_columnsMany", "delete_columns", "delete_columnsMany", "sort_rows", "sort_rowsMany", "fill", "fillMany", "apply_edit", "apply_editMany", "comment", "delete_comment", "delete_commentMany", "set_settings", "set_settingsMany", "import_table", "import_tableMany", "read_content", "export_workbook", "export_ods", "export_table", "edit_cells", "edit_cellsMany"] as const;

/** The `tl.so.tilo.sheets.Workbook` kind at `path`. */
export function workbook(ws: tilo.Workspace, path: string): Workbook {
  return resourceProxy(ws, path, SURFACE, WORKBOOK_ACTIONS) as unknown as Workbook;
}

/** Sheet — `tl.so.tilo.sheets.Sheet`. */
export interface Sheet extends tilo.Resource {
  l36xpi4o: string;  // name
  hotvvuda: string;  // source
  "6jss3eyn": number | null;  // color
  "6xebiw3n": Record<string, string>;  // cells
  s2rtlgut: Record<string, string>;  // formats
  vy3vanj2: string[];  // bold
  d2szrgfm: Record<string, string>;  // widths
  km7cpljo: Record<string, string>;  // heights
  lsltfxgz: Array<tilo.Entry<Chart>>;  // charts
  "5lw5xfqk": boolean;  // hidden
  /** Hides a sheet's tab, or shows it again. */
  set_sheet_hidden(args: {
    hidden: boolean;
  }): Promise<void>;
  /** Runs `set_sheet_hidden` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_sheet_hiddenMany(items: SetSheetHiddenArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Gives a sheet's tab a colour from the palette, by its place in it. */
  set_sheet_color(args: {
    color: number;
  }): Promise<void>;
  /** Runs `set_sheet_color` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_sheet_colorMany(items: SetSheetColorArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Gives a sheet's tab no colour of its own, so it takes the theme's. */
  clear_sheet_color(): Promise<void>;
  /** Runs `clear_sheet_color` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_sheet_colorMany(items: ClearSheetColorArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Charts the cells `range` as a column or a line chart, titled `title`. */
  add_chart(args: {
    range: string;
    kind: ChartKind;
    title: string;
  }): Promise<Chart>;
  /** Deletes a chart. */
  delete_chart(args: {
    c: Chart;
  }): Promise<void>;
  /** Runs `delete_chart` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_chartMany(items: DeleteChartArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps the width the person dragged a column to: the pair carries its letter and its width. */
  resize_column(args: {
    pair: string[];
  }): Promise<void>;
  /** Runs `resize_column` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  resize_columnMany(items: ResizeColumnArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps the height the person dragged a row to: the pair carries its number and its height. */
  resize_row(args: {
    pair: string[];
  }): Promise<void>;
  /** Runs `resize_row` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  resize_rowMany(items: ResizeRowArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Sheet` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SHEET_ACTIONS = ["set_sheet_hidden", "set_sheet_hiddenMany", "set_sheet_color", "set_sheet_colorMany", "clear_sheet_color", "clear_sheet_colorMany", "add_chart", "delete_chart", "delete_chartMany", "resize_column", "resize_columnMany", "resize_row", "resize_rowMany"] as const;

/** The `tl.so.tilo.sheets.Sheet` kind at `path`. */
export function sheet(ws: tilo.Workspace, path: string): Sheet {
  return resourceProxy(ws, path, SURFACE, SHEET_ACTIONS) as unknown as Sheet;
}

/** Chart — `tl.so.tilo.sheets.Chart`. */
export interface Chart extends tilo.Resource {
  iipmzvk6: string;  // title
  todwcigp: ChartKind;  // kind
  urs2nvtf: string;  // range
  /** Changes a chart: its title, and its kind or the cells it charts when given. */
  edit_chart(args?: {
    title?: string;
    kind?: ChartKind;
    range?: string;
  }): Promise<void>;
  /** Runs `edit_chart` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  edit_chartMany(items: EditChartArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Chart` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CHART_ACTIONS = ["edit_chart", "edit_chartMany"] as const;

/** The `tl.so.tilo.sheets.Chart` kind at `path`. */
export function chart(ws: tilo.Workspace, path: string): Chart {
  return resourceProxy(ws, path, SURFACE, CHART_ACTIONS) as unknown as Chart;
}

/** Comment — `tl.so.tilo.sheets.Comment`. */
export interface Comment extends tilo.Resource {
  fhfswroi: string;  // sheet
  lfxsmldw: string;  // range
  "437c6ryq": string;  // text
  nosjmdwd: boolean;  // resolved
  "64zcrzt6": Array<tilo.Entry<Reply>>;  // replies
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
  /** Answers a comment. */
  reply(args: {
    text: string;
  }): Promise<Reply>;
  /** Resolves a comment, or opens it again. */
  resolve(args?: {
    resolved?: boolean;
  }): Promise<void>;
  /** Runs `resolve` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  resolveMany(items: ResolveArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Changes what a comment says. Only its author may. */
  edit_comment(args?: {
    text?: string;
  }): Promise<void>;
  /** Runs `edit_comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  edit_commentMany(items: EditCommentArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Comment` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const COMMENT_ACTIONS = ["editBlock", "editBlockMany", "insertBlock", "insertBlockMany", "deleteBlock", "deleteBlockMany", "reply", "resolve", "resolveMany", "edit_comment", "edit_commentMany"] as const;

/** The `tl.so.tilo.sheets.Comment` kind at `path`. */
export function comment(ws: tilo.Workspace, path: string): Comment {
  return resourceProxy(ws, path, SURFACE, COMMENT_ACTIONS) as unknown as Comment;
}

/** Reply — `tl.so.tilo.sheets.Reply`. */
export interface Reply extends tilo.Resource {
  rehztzuy: string;  // text
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
  /** Changes what a reply says. Only its author may. */
  edit_reply(args?: {
    text?: string;
  }): Promise<void>;
  /** Runs `edit_reply` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  edit_replyMany(items: EditReplyArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Reply` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REPLY_ACTIONS = ["editBlock", "editBlockMany", "insertBlock", "insertBlockMany", "deleteBlock", "deleteBlockMany", "edit_reply", "edit_replyMany"] as const;

/** The `tl.so.tilo.sheets.Reply` kind at `path`. */
export function reply(ws: tilo.Workspace, path: string): Reply {
  return resourceProxy(ws, path, SURFACE, REPLY_ACTIONS) as unknown as Reply;
}

/** SheetInput — `tl.so.tilo.sheets.SheetInput`. */
export interface SheetInput extends tilo.Resource {
  d4n4c4j6: string;  // name
  iitrsrcd: boolean;  // hidden
  owwnyfq7: Record<string, string>;  // cells
  ftagyrff: Record<string, string>;  // formats
  xrjhhdys: string[];  // bold
}

/** The actions `SheetInput` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SHEETINPUT_ACTIONS = [] as const;

/** The `tl.so.tilo.sheets.SheetInput` kind at `path`. */
export function sheetInput(ws: tilo.Workspace, path: string): SheetInput {
  return resourceProxy(ws, path, SURFACE, SHEETINPUT_ACTIONS) as unknown as SheetInput;
}

/** BookInput — `tl.so.tilo.sheets.BookInput`. */
export interface BookInput extends tilo.Resource {
  z2cfbg2g: SheetInput[];  // sheets
  ktfpf6rx: string;  // currency
}

/** The actions `BookInput` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const BOOKINPUT_ACTIONS = [] as const;

/** The `tl.so.tilo.sheets.BookInput` kind at `path`. */
export function bookInput(ws: tilo.Workspace, path: string): BookInput {
  return resourceProxy(ws, path, SURFACE, BOOKINPUT_ACTIONS) as unknown as BookInput;
}

/** RecordsRequest — `tl.so.tilo.sheets.RecordsRequest`. */
export interface RecordsRequest extends tilo.Resource {
  quawedop: Array<Record<string, string>>;  // records
  uslgkxn3: number | null;  // row
  vstmdg7x: string[] | null;  // columns
  bou7m624: string | null;  // key
}

/** The actions `RecordsRequest` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RECORDSREQUEST_ACTIONS = [] as const;

/** The `tl.so.tilo.sheets.RecordsRequest` kind at `path`. */
export function recordsRequest(ws: tilo.Workspace, path: string): RecordsRequest {
  return resourceProxy(ws, path, SURFACE, RECORDSREQUEST_ACTIONS) as unknown as RecordsRequest;
}

/** UpsertRequest — `tl.so.tilo.sheets.UpsertRequest`. */
export interface UpsertRequest extends tilo.Resource {
  styf7w4a: Array<Record<string, string>>;  // records
  s3tgj4q3: Record<string, string>;  // matching
  wlypu6mi: Record<string, string>;  // value
}

/** The actions `UpsertRequest` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const UPSERTREQUEST_ACTIONS = [] as const;

/** The `tl.so.tilo.sheets.UpsertRequest` kind at `path`. */
export function upsertRequest(ws: tilo.Workspace, path: string): UpsertRequest {
  return resourceProxy(ws, path, SURFACE, UPSERTREQUEST_ACTIONS) as unknown as UpsertRequest;
}

/** RecordWrites — `tl.so.tilo.sheets.RecordWrites`. */
export interface RecordWrites extends tilo.Resource {
  tloxk2t5: Record<string, string>;  // cells
  chuotih4: Record<string, string>;  // formats
}

/** The actions `RecordWrites` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RECORDWRITES_ACTIONS = [] as const;

/** The `tl.so.tilo.sheets.RecordWrites` kind at `path`. */
export function recordWrites(ws: tilo.Workspace, path: string): RecordWrites {
  return resourceProxy(ws, path, SURFACE, RECORDWRITES_ACTIONS) as unknown as RecordWrites;
}

/** Extent — `tl.so.tilo.sheets.Extent`. */
export interface Extent extends tilo.Resource {
  "5pdxnqaj": number;  // columns
  hmh5rxxy: number;  // rows
}

/** The actions `Extent` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const EXTENT_ACTIONS = [] as const;

/** The `tl.so.tilo.sheets.Extent` kind at `path`. */
export function extent(ws: tilo.Workspace, path: string): Extent {
  return resourceProxy(ws, path, SURFACE, EXTENT_ACTIONS) as unknown as Extent;
}

/** Sheets — `tl.so.tilo.sheets`. */
export interface Sheets extends tilo.Resource {
  /** Copies the cells a person chose to their clipboard as a table: a line per row and a tab between columns, which a sheet, a word processor or a mail app reads. */
  copy_cells(args: {
    text: string;
  }): Promise<void>;
  /** What a spreadsheet file holds, as the sheets the app keeps: each sheet's name, whether its tab is hidden, and its cells as typed by their A1 reference. An agent reads a table without the window. */
  read_file(args: {
    file: unknown;
  }): Promise<unknown>;
}

/** The actions `Sheets` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SHEETS_ACTIONS = ["copy_cells", "read_file"] as const;

/** The `tl.so.tilo.sheets` kind at `path`. */
export function sheets(ws: tilo.Workspace, path: string): Sheets {
  return resourceProxy(ws, path, SURFACE, SHEETS_ACTIONS) as unknown as Sheets;
}

