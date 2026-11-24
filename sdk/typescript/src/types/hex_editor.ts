/**
 * Types for Hex Editor (`so.tilo.hex-editor`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Change` is a `Resource` of the app's `tl.so.tilo.hex-editor.Change`, made by its factory with the
 * resource's path (`hex-editor/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.hex-editor@35422bf2b2064fc9";

/** The arguments of `show`. */
export interface ShowArgs {
}

/** The arguments of `save`. */
export interface SaveArgs {
}

/** The arguments of `rebased`. */
export interface RebasedArgs {
  address: string;
  length: number;
}

/** The arguments of `moved`. */
export interface MovedArgs {
  desk: Desk;
  at: number;
  extend: boolean;
}

/** The arguments of `type_key`. */
export interface TypeKeyArgs {
  desk: Desk;
  one: string;
}

/** The arguments of `type_hex`. */
export interface TypeHexArgs {
  desk: Desk;
  value: number;
}

/** The arguments of `type_ascii`. */
export interface TypeAsciiArgs {
  desk: Desk;
  one: string;
}

/** The arguments of `backspace`. */
export interface BackspaceArgs {
  desk: Desk;
}

/** The arguments of `delete_at_cursor`. */
export interface DeleteAtCursorArgs {
  desk: Desk;
}

/** The arguments of `select_all`. */
export interface SelectAllArgs {
  desk: Desk;
}

/** The arguments of `paste`. */
export interface PasteArgs {
}

/** The arguments of `replaced`. */
export interface ReplacedArgs {
  desk: Desk;
  spans: string[];
  bytes: string;
}

/** The arguments of `start_search`. */
export interface StartSearchArgs {
}

/** The arguments of `start_replace`. */
export interface StartReplaceArgs {
}

/** The arguments of `start_goto`. */
export interface StartGotoArgs {
}

/** The arguments of `toggle_inspector`. */
export interface ToggleInspectorArgs {
}

/** The arguments of `close_panel`. */
export interface ClosePanelArgs {
}

/** The arguments of `search`. */
export interface SearchArgs {
  query: string;
}

/** The arguments of `reveal`. */
export interface RevealArgs {
  index: number;
}

/** The arguments of `replace_all`. */
export interface ReplaceAllArgs {
  with: string;
}

/** The arguments of `replace_spans`. */
export interface ReplaceSpansArgs {
  desk: Desk;
  spans: string[];
  with: string;
}

/** The arguments of `search_kind`. */
export interface SearchKindArgs {
  i: number;
}

/** The arguments of `search_case`. */
export interface SearchCaseArgs {
  on: boolean;
}

/** The arguments of `search_word`. */
export interface SearchWordArgs {
  on: boolean;
}

/** The arguments of `go_to`. */
export interface GoToArgs {
  query: string;
}

/** The arguments of `set_endian`. */
export interface SetEndianArgs {
  i: number;
}

/** The arguments of `set_mode`. */
export interface SetModeArgs {
  i: number;
}

/** The arguments of `set_column`. */
export interface SetColumnArgs {
  i: number;
}

/** The arguments of `dismiss`. */
export interface DismissArgs {
}

/** The arguments of `pressed`. */
export interface PressedArgs {
  at: string[];
}

/** The arguments of `scrolled`. */
export interface ScrolledArgs {
  rows: number;
}

/** The arguments of `insert_bytes`. */
export interface InsertBytesArgs {
  offset: number;
  bytes: string;
}

/** The arguments of `overwrite_bytes`. */
export interface OverwriteBytesArgs {
  offset: number;
  bytes: string;
}

/** The arguments of `delete_bytes`. */
export interface DeleteBytesArgs {
  offset: number;
  len: number;
}

/** The arguments of `forget`. */
export interface ForgetArgs {
  d: Document;
}

/** Change — `tl.so.tilo.hex-editor.Change`. */
export interface Change extends tilo.Resource {
  b4ynsjfc: number;  // at
  lkghkw33: number;  // remove
  n55erru5: string;  // insert
}

/** The actions `Change` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CHANGE_ACTIONS = [] as const;

/** The `tl.so.tilo.hex-editor.Change` kind at `path`. */
export function change(ws: tilo.Workspace, path: string): Change {
  return resourceProxy(ws, path, SURFACE, CHANGE_ACTIONS) as unknown as Change;
}

/** Row — `tl.so.tilo.hex-editor.Row`. */
export interface Row extends tilo.Resource {
  rqqjo5b2: string;  // hex
  i23efoqg: string;  // changed
}

/** The actions `Row` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ROW_ACTIONS = [] as const;

/** The `tl.so.tilo.hex-editor.Row` kind at `path`. */
export function row(ws: tilo.Workspace, path: string): Row {
  return resourceProxy(ws, path, SURFACE, ROW_ACTIONS) as unknown as Row;
}

/** Finding — `tl.so.tilo.hex-editor.Finding`. */
export interface Finding extends tilo.Resource {
  j3rx5nr6: string;  // label
  bqtm2sxd: string;  // value
  whqswnrc: number;  // width
}

/** The actions `Finding` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FINDING_ACTIONS = [] as const;

/** The `tl.so.tilo.hex-editor.Finding` kind at `path`. */
export function finding(ws: tilo.Workspace, path: string): Finding {
  return resourceProxy(ws, path, SURFACE, FINDING_ACTIONS) as unknown as Finding;
}

/** Span — `tl.so.tilo.hex-editor.Span`. */
export interface Span extends tilo.Resource {
  vjv7m35i: number;  // start
  "6nu52vd6": number;  // end
}

/** The actions `Span` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SPAN_ACTIONS = [] as const;

/** The `tl.so.tilo.hex-editor.Span` kind at `path`. */
export function span(ws: tilo.Workspace, path: string): Span {
  return resourceProxy(ws, path, SURFACE, SPAN_ACTIONS) as unknown as Span;
}

/** Desk — `tl.so.tilo.hex-editor.Desk`. */
export interface Desk extends tilo.Resource {
  "4pp4s3sf": number;  // cursor
  ta3nqlbq: number | null;  // anchor
  d6nzpsjf: string;  // column
  mum5d7nq: string;  // mode
  xmgji4li: number | null;  // high
  b23s2qvq: string;  // endian
  ntd56cpd: number;  // first
  d4rmfnak: number;  // columns
  u54h5izd: string[];  // rows
  iyf5x4du: string[];  // changed
  nczhspum: string[];  // findings
  uswoxemn: string[];  // hits
  nv3vcmev: number;  // at_match
  s777zrl4: boolean;  // truncated
  gy4cijiy: string;  // query
  hfchq6ug: string;  // kind
  pzwozqbg: boolean;  // match_case
  olbisfbn: boolean;  // whole_word
  jko5an6z: string;  // panel
  n7ibv25j: string;  // notice
  jslwh26v: string;  // notice_kind
  /** Say something in the window: a result, or what went wrong. */
  say(args: {
    words: string;
    kind: string;
  }): Promise<boolean>;
}

/** The actions `Desk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DESK_ACTIONS = ["say"] as const;

/** The `tl.so.tilo.hex-editor.Desk` kind at `path`. */
export function desk(ws: tilo.Workspace, path: string): Desk {
  return resourceProxy(ws, path, SURFACE, DESK_ACTIONS) as unknown as Desk;
}

/** Document — `tl.so.tilo.hex-editor.Document`. */
export interface Document extends tilo.Resource {
  dbkdqzos: string;  // name
  teahiasd: number;  // opened
  blhbkxjc: string;  // base
  wts2l2hg: string;  // path
  nqk5afwc: number;  // length
  "7u75gmtu": number;  // edited
  "3tqozdp3": string[];  // changes
  "2fs4ukcj": Array<tilo.Entry<Desk>>;  // desks
  /** The cursor moved to `at`, kept inside the bytes; `extend` keeps where the selection started, and the window's first row follows it when it leaves the rows shown. */
  move_cursor(args: {
    desk: Desk;
    at: number;
    extend: boolean;
  }): Promise<boolean>;
  /** Read the window's rows and the bytes at its cursor from the device again. */
  show(): Promise<void>;
  /** Runs `show` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  showMany(items: ShowArgs[]): Promise<tilo.ActionAnswer[]>;
  // the app's `edit` action is the SDK's own: run it with `action("edit")`.
  /** Saves the buffer where it came from, or asks where when it stands nowhere yet. */
  save(): Promise<void>;
  /** Runs `save` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  saveMany(items: SaveArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The bytes as a file the person places with the system's save chooser, which the window shows. */
  save_bytes(): Promise<unknown>;
  /** What was saved is the base: no edit stands against it any more. */
  rebased(args: {
    address: string;
    length: number;
  }): Promise<void>;
  /** Runs `rebased` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rebasedMany(items: RebasedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The keys pressed on the grid, as the person's keyboard names them. */
  press(args: {
    key: string;
  }): Promise<void>;
  /** The Ctrl chords typed while the bytes have the keyboard. */
  chord(args: {
    desk: Desk;
    key: string;
  }): Promise<void>;
  /** The cursor moved to `at`: the rows and the inspector read there. */
  moved(args: {
    desk: Desk;
    at: number;
    extend: boolean;
  }): Promise<void>;
  /** Runs `moved` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  movedMany(items: MovedArgs[]): Promise<tilo.ActionAnswer[]>;
  type_key(args: {
    desk: Desk;
    one: string;
  }): Promise<void>;
  /** Runs `type_key` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  type_keyMany(items: TypeKeyArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Type one hexadecimal digit: in overwrite mode the first sets the high nibble and the second completes the byte; in insert mode each completed byte is spliced in. */
  type_hex(args: {
    desk: Desk;
    value: number;
  }): Promise<void>;
  /** Runs `type_hex` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  type_hexMany(items: TypeHexArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Type a character into the ASCII column, then advance. */
  type_ascii(args: {
    desk: Desk;
    one: string;
  }): Promise<void>;
  /** Runs `type_ascii` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  type_asciiMany(items: TypeAsciiArgs[]): Promise<tilo.ActionAnswer[]>;
  /** In insert mode the byte before the cursor goes; in overwrite mode the cursor only steps back. */
  backspace(args: {
    desk: Desk;
  }): Promise<void>;
  /** Runs `backspace` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  backspaceMany(items: BackspaceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The selection goes, or else the byte under the cursor. */
  delete_at_cursor(args: {
    desk: Desk;
  }): Promise<void>;
  /** Runs `delete_at_cursor` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_at_cursorMany(items: DeleteAtCursorArgs[]): Promise<tilo.ActionAnswer[]>;
  select_all(args: {
    desk: Desk;
  }): Promise<void>;
  /** Runs `select_all` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  select_allMany(items: SelectAllArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Copies the selected bytes in the column's form: `50 4e 47 44`, or `PNGD`. */
  copy(): Promise<void>;
  /** Copies the selection and deletes it. */
  cut(): Promise<void>;
  /** Pastes the clipboard at the selection, or at the cursor, in the focus mode. */
  paste(): Promise<void>;
  /** Runs `paste` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  pasteMany(items: PasteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Replaces each span with the same bytes, as one step. */
  replaced(args: {
    desk: Desk;
    spans: string[];
    bytes: string;
  }): Promise<void>;
  /** Runs `replaced` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  replacedMany(items: ReplacedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens the search strip. */
  start_search(): Promise<void>;
  /** Runs `start_search` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  start_searchMany(items: StartSearchArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens the replace strip. */
  start_replace(): Promise<void>;
  /** Runs `start_replace` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  start_replaceMany(items: StartReplaceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens the go-to-offset strip. */
  start_goto(): Promise<void>;
  /** Runs `start_goto` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  start_gotoMany(items: StartGotoArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows or hides the inspector. */
  toggle_inspector(): Promise<void>;
  /** Runs `toggle_inspector` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  toggle_inspectorMany(items: ToggleInspectorArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Closes the strip under the bar. */
  close_panel(): Promise<void>;
  /** Runs `close_panel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  close_panelMany(items: ClosePanelArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Finds `query` in the bytes: a hexadecimal pattern, or text under its rules. */
  search(args: {
    query: string;
  }): Promise<void>;
  /** Runs `search` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  searchMany(items: SearchArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts the cursor on the match at `index`. */
  reveal(args: {
    index: number;
  }): Promise<void>;
  /** Runs `reveal` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  revealMany(items: RevealArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Replaces every match the search found with `with`. */
  replace_all(args: {
    with: string;
  }): Promise<void>;
  /** Runs `replace_all` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  replace_allMany(items: ReplaceAllArgs[]): Promise<tilo.ActionAnswer[]>;
  replace_spans(args: {
    desk: Desk;
    spans: string[];
    with: string;
  }): Promise<void>;
  /** Runs `replace_spans` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  replace_spansMany(items: ReplaceSpansArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reads the search strip's choices: text or hexadecimal, letter case and whole words. */
  search_kind(args: {
    i: number;
  }): Promise<void>;
  /** Runs `search_kind` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  search_kindMany(items: SearchKindArgs[]): Promise<tilo.ActionAnswer[]>;
  search_case(args: {
    on: boolean;
  }): Promise<void>;
  /** Runs `search_case` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  search_caseMany(items: SearchCaseArgs[]): Promise<tilo.ActionAnswer[]>;
  search_word(args: {
    on: boolean;
  }): Promise<void>;
  /** Runs `search_word` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  search_wordMany(items: SearchWordArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves the cursor to the offset `query` names. */
  go_to(args: {
    query: string;
  }): Promise<void>;
  /** Runs `go_to` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  go_toMany(items: GoToArgs[]): Promise<tilo.ActionAnswer[]>;
  set_endian(args: {
    i: number;
  }): Promise<void>;
  /** Runs `set_endian` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_endianMany(items: SetEndianArgs[]): Promise<tilo.ActionAnswer[]>;
  set_mode(args: {
    i: number;
  }): Promise<void>;
  /** Runs `set_mode` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_modeMany(items: SetModeArgs[]): Promise<tilo.ActionAnswer[]>;
  set_column(args: {
    i: number;
  }): Promise<void>;
  /** Runs `set_column` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_columnMany(items: SetColumnArgs[]): Promise<tilo.ActionAnswer[]>;
  dismiss(): Promise<void>;
  /** Runs `dismiss` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  dismissMany(items: DismissArgs[]): Promise<tilo.ActionAnswer[]>;
  /** A press on the grid: the byte at `at` gets the cursor, the column pressed the focus, and `'shift'` keeps where the selection started. */
  pressed(args: {
    at: string[];
  }): Promise<void>;
  /** Runs `pressed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  pressedMany(items: PressedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The wheel or the bar moved the window by `rows`. */
  scrolled(args: {
    rows: number;
  }): Promise<void>;
  /** Runs `scrolled` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  scrolledMany(items: ScrolledArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Inserts `bytes` (hexadecimal, like `de ad be ef`) at `offset`. */
  insert_bytes(args: {
    offset: number;
    bytes: string;
  }): Promise<void>;
  /** Runs `insert_bytes` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  insert_bytesMany(items: InsertBytesArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Replaces the bytes at `offset` with `bytes` (hexadecimal), never growing the file. */
  overwrite_bytes(args: {
    offset: number;
    bytes: string;
  }): Promise<void>;
  /** Runs `overwrite_bytes` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  overwrite_bytesMany(items: OverwriteBytesArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Removes `len` bytes at `offset`. */
  delete_bytes(args: {
    offset: number;
    len: number;
  }): Promise<void>;
  /** Runs `delete_bytes` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_bytesMany(items: DeleteBytesArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Document` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DOCUMENT_ACTIONS = ["move_cursor", "show", "showMany", "save", "saveMany", "save_bytes", "rebased", "rebasedMany", "press", "chord", "moved", "movedMany", "type_key", "type_keyMany", "type_hex", "type_hexMany", "type_ascii", "type_asciiMany", "backspace", "backspaceMany", "delete_at_cursor", "delete_at_cursorMany", "select_all", "select_allMany", "copy", "cut", "paste", "pasteMany", "replaced", "replacedMany", "start_search", "start_searchMany", "start_replace", "start_replaceMany", "start_goto", "start_gotoMany", "toggle_inspector", "toggle_inspectorMany", "close_panel", "close_panelMany", "search", "searchMany", "reveal", "revealMany", "replace_all", "replace_allMany", "replace_spans", "replace_spansMany", "search_kind", "search_kindMany", "search_case", "search_caseMany", "search_word", "search_wordMany", "go_to", "go_toMany", "set_endian", "set_endianMany", "set_mode", "set_modeMany", "set_column", "set_columnMany", "dismiss", "dismissMany", "pressed", "pressedMany", "scrolled", "scrolledMany", "insert_bytes", "insert_bytesMany", "overwrite_bytes", "overwrite_bytesMany", "delete_bytes", "delete_bytesMany"] as const;

/** The `tl.so.tilo.hex-editor.Document` kind at `path`. */
export function hexEditorDocument(ws: tilo.Workspace, path: string): Document {
  return resourceProxy(ws, path, SURFACE, DOCUMENT_ACTIONS) as unknown as Document;
}

/** Library — `tl.so.tilo.hex-editor.Library`. */
export interface Library extends tilo.Resource {
  fu57pivb: Array<tilo.Entry<Document>>;  // documents
  /** Opens the file at `path` on this device as its own buffer. */
  open_path(args: {
    path: string;
  }): Promise<Document>;
  refused_buffer(args: {
    path: string;
    why: string;
  }): Promise<Document>;
  /** Opens a file a person chose, or one another app handed over, as its own buffer. Its bytes are the address the content store gave them, so every device reads the same file. */
  open_file(args: {
    file: unknown;
  }): Promise<Document>;
  /** The buffer of the bytes at `address`: the one already open on them, read afresh, or a new one. */
  opened_buffer(args: {
    name: string;
    address: string;
    path: string;
    length: number;
  }): Promise<Document>;
  /** An empty buffer to type bytes into, saved wherever the person says. */
  new_buffer(): Promise<Document>;
  empty_buffer(args: {
    name: string;
  }): Promise<Document>;
  /** The action the person's file chooser fills. */
  open_chosen(args: {
    file: unknown;
  }): Promise<Document>;
  /** Forgets a buffer, its edits with it. */
  forget(args: {
    d: Document;
  }): Promise<void>;
  /** Runs `forget` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  forgetMany(items: ForgetArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Library` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LIBRARY_ACTIONS = ["open_path", "refused_buffer", "open_file", "opened_buffer", "new_buffer", "empty_buffer", "open_chosen", "forget", "forgetMany"] as const;

/** The `tl.so.tilo.hex-editor.Library` kind at `path`. */
export function library(ws: tilo.Workspace, path: string): Library {
  return resourceProxy(ws, path, SURFACE, LIBRARY_ACTIONS) as unknown as Library;
}

/** OpenAsk — `tl.so.tilo.hex-editor.OpenAsk`. */
export interface OpenAsk extends tilo.Resource {
  rruucmz4: string;  // path
}

/** The actions `OpenAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OPENASK_ACTIONS = [] as const;

/** The `tl.so.tilo.hex-editor.OpenAsk` kind at `path`. */
export function openAsk(ws: tilo.Workspace, path: string): OpenAsk {
  return resourceProxy(ws, path, SURFACE, OPENASK_ACTIONS) as unknown as OpenAsk;
}

/** OpenedAnswer — `tl.so.tilo.hex-editor.OpenedAnswer`. */
export interface OpenedAnswer extends tilo.Resource {
  cevle23p: string;  // name
  idpg6pxk: string;  // address
  cr3tncji: number;  // length
}

/** The actions `OpenedAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OPENEDANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.hex-editor.OpenedAnswer` kind at `path`. */
export function openedAnswer(ws: tilo.Workspace, path: string): OpenedAnswer {
  return resourceProxy(ws, path, SURFACE, OPENEDANSWER_ACTIONS) as unknown as OpenedAnswer;
}

/** RowsAsk — `tl.so.tilo.hex-editor.RowsAsk`. */
export interface RowsAsk extends tilo.Resource {
  ldq6mtad: string;  // address
  "2t4i36rg": string[];  // changes
  "52p24f3b": number;  // first
  j3ztka6c: number;  // count
  jmwhucjc: number;  // columns
  tjv2yhp3: string;  // endian
  oowz47v3: number;  // cursor
}

/** The actions `RowsAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ROWSASK_ACTIONS = [] as const;

/** The `tl.so.tilo.hex-editor.RowsAsk` kind at `path`. */
export function rowsAsk(ws: tilo.Workspace, path: string): RowsAsk {
  return resourceProxy(ws, path, SURFACE, ROWSASK_ACTIONS) as unknown as RowsAsk;
}

/** RowsAnswer — `tl.so.tilo.hex-editor.RowsAnswer`. */
export interface RowsAnswer extends tilo.Resource {
  yjjtzhyt: number;  // length
  ao74hr7t: number;  // edited
  lgghdfh2: boolean;  // read
  df32ywi4: string;  // error
  jhbkwvr2: string[];  // rows
  "3ugkwm42": string[];  // changed
  xhxdxsgb: string[];  // findings
}

/** The actions `RowsAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ROWSANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.hex-editor.RowsAnswer` kind at `path`. */
export function rowsAnswer(ws: tilo.Workspace, path: string): RowsAnswer {
  return resourceProxy(ws, path, SURFACE, ROWSANSWER_ACTIONS) as unknown as RowsAnswer;
}

/** FindAsk — `tl.so.tilo.hex-editor.FindAsk`. */
export interface FindAsk extends tilo.Resource {
  "6grc7o75": string;  // address
  "5yumynmf": string[];  // changes
  ljornfmm: string;  // query
  wyvxlbd4: string;  // kind
  b4bt6mtj: boolean;  // case
  bxix7im4: boolean;  // word
  qyar3ymw: number;  // limit
}

/** The actions `FindAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FINDASK_ACTIONS = [] as const;

/** The `tl.so.tilo.hex-editor.FindAsk` kind at `path`. */
export function findAsk(ws: tilo.Workspace, path: string): FindAsk {
  return resourceProxy(ws, path, SURFACE, FINDASK_ACTIONS) as unknown as FindAsk;
}

/** FoundAnswer — `tl.so.tilo.hex-editor.FoundAnswer`. */
export interface FoundAnswer extends tilo.Resource {
  t6d5sfoz: string[];  // hits
  "4fdr6kla": boolean;  // truncated
}

/** The actions `FoundAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FOUNDANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.hex-editor.FoundAnswer` kind at `path`. */
export function foundAnswer(ws: tilo.Workspace, path: string): FoundAnswer {
  return resourceProxy(ws, path, SURFACE, FOUNDANSWER_ACTIONS) as unknown as FoundAnswer;
}

/** ApplyAsk — `tl.so.tilo.hex-editor.ApplyAsk`. */
export interface ApplyAsk extends tilo.Resource {
  ba424tqt: string;  // address
  qtf35usu: string[];  // changes
  r5xf3t6m: string;  // op
  z2ahkv3u: number;  // at
  "3kwri4q5": number;  // len
  pk7jqula: string;  // bytes
}

/** The actions `ApplyAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const APPLYASK_ACTIONS = [] as const;

/** The `tl.so.tilo.hex-editor.ApplyAsk` kind at `path`. */
export function applyAsk(ws: tilo.Workspace, path: string): ApplyAsk {
  return resourceProxy(ws, path, SURFACE, APPLYASK_ACTIONS) as unknown as ApplyAsk;
}

/** AppliedAnswer — `tl.so.tilo.hex-editor.AppliedAnswer`. */
export interface AppliedAnswer extends tilo.Resource {
  "2w7szbth": string;  // change
  ll4ibsb5: number;  // length
}

/** The actions `AppliedAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const APPLIEDANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.hex-editor.AppliedAnswer` kind at `path`. */
export function appliedAnswer(ws: tilo.Workspace, path: string): AppliedAnswer {
  return resourceProxy(ws, path, SURFACE, APPLIEDANSWER_ACTIONS) as unknown as AppliedAnswer;
}

/** ReplaceAsk — `tl.so.tilo.hex-editor.ReplaceAsk`. */
export interface ReplaceAsk extends tilo.Resource {
  "3riih5ln": string;  // address
  o4kbz7cm: string[];  // changes
  ptnk64f6: string[];  // spans
  us73s4xt: string;  // bytes
  mj26u2qk: string;  // mode
}

/** The actions `ReplaceAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REPLACEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.hex-editor.ReplaceAsk` kind at `path`. */
export function replaceAsk(ws: tilo.Workspace, path: string): ReplaceAsk {
  return resourceProxy(ws, path, SURFACE, REPLACEASK_ACTIONS) as unknown as ReplaceAsk;
}

/** ReplacedAnswer — `tl.so.tilo.hex-editor.ReplacedAnswer`. */
export interface ReplacedAnswer extends tilo.Resource {
  j3lt4zli: string[];  // changes
  "3kaouk56": number;  // count
  "4fce7y46": number;  // length
}

/** The actions `ReplacedAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REPLACEDANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.hex-editor.ReplacedAnswer` kind at `path`. */
export function replacedAnswer(ws: tilo.Workspace, path: string): ReplacedAnswer {
  return resourceProxy(ws, path, SURFACE, REPLACEDANSWER_ACTIONS) as unknown as ReplacedAnswer;
}

/** RangeAsk — `tl.so.tilo.hex-editor.RangeAsk`. */
export interface RangeAsk extends tilo.Resource {
  "3surtarm": string;  // address
  l7dzgaqe: string[];  // changes
  "3v66hb5v": number;  // start
  fsjcytnx: number;  // end
  fyp52l2r: string;  // kind
}

/** The actions `RangeAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RANGEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.hex-editor.RangeAsk` kind at `path`. */
export function rangeAsk(ws: tilo.Workspace, path: string): RangeAsk {
  return resourceProxy(ws, path, SURFACE, RANGEASK_ACTIONS) as unknown as RangeAsk;
}

/** RangeAnswer — `tl.so.tilo.hex-editor.RangeAnswer`. */
export interface RangeAnswer extends tilo.Resource {
  d7nxzpqz: string;  // text
}

/** The actions `RangeAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RANGEANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.hex-editor.RangeAnswer` kind at `path`. */
export function rangeAnswer(ws: tilo.Workspace, path: string): RangeAnswer {
  return resourceProxy(ws, path, SURFACE, RANGEANSWER_ACTIONS) as unknown as RangeAnswer;
}

/** BytesAsk — `tl.so.tilo.hex-editor.BytesAsk`. */
export interface BytesAsk extends tilo.Resource {
  "66cpqgom": string;  // address
  "4tyyvxqh": string[];  // changes
}

/** The actions `BytesAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const BYTESASK_ACTIONS = [] as const;

/** The `tl.so.tilo.hex-editor.BytesAsk` kind at `path`. */
export function bytesAsk(ws: tilo.Workspace, path: string): BytesAsk {
  return resourceProxy(ws, path, SURFACE, BYTESASK_ACTIONS) as unknown as BytesAsk;
}

/** BytesAnswer — `tl.so.tilo.hex-editor.BytesAnswer`. */
export interface BytesAnswer extends tilo.Resource {
  smxqtshm: string;  // bytes
}

/** The actions `BytesAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const BYTESANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.hex-editor.BytesAnswer` kind at `path`. */
export function bytesAnswer(ws: tilo.Workspace, path: string): BytesAnswer {
  return resourceProxy(ws, path, SURFACE, BYTESANSWER_ACTIONS) as unknown as BytesAnswer;
}

/** SaveAsk — `tl.so.tilo.hex-editor.SaveAsk`. */
export interface SaveAsk extends tilo.Resource {
  "62acy6x2": string;  // address
  "2d44lw44": string[];  // changes
  sujs7lc2: string;  // path
}

/** The actions `SaveAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SAVEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.hex-editor.SaveAsk` kind at `path`. */
export function saveAsk(ws: tilo.Workspace, path: string): SaveAsk {
  return resourceProxy(ws, path, SURFACE, SAVEASK_ACTIONS) as unknown as SaveAsk;
}

/** SavedAnswer — `tl.so.tilo.hex-editor.SavedAnswer`. */
export interface SavedAnswer extends tilo.Resource {
  fnprm7qy: string;  // address
  bnwrfq64: number;  // length
}

/** The actions `SavedAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SAVEDANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.hex-editor.SavedAnswer` kind at `path`. */
export function savedAnswer(ws: tilo.Workspace, path: string): SavedAnswer {
  return resourceProxy(ws, path, SURFACE, SAVEDANSWER_ACTIONS) as unknown as SavedAnswer;
}

/** Hex Editor — `tl.so.tilo.hex-editor`. */
export interface HexEditor extends tilo.Resource {
}

/** The actions `HexEditor` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const HEXEDITOR_ACTIONS = [] as const;

/** The `tl.so.tilo.hex-editor` kind at `path`. */
export function hexEditor(ws: tilo.Workspace, path: string): HexEditor {
  return resourceProxy(ws, path, SURFACE, HEXEDITOR_ACTIONS) as unknown as HexEditor;
}

