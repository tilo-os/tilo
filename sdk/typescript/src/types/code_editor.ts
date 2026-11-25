/**
 * Types for Code Editor (`so.tilo.code-editor`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`code-editor/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.code-editor@6929cbdec37a6d37";

/** What an entry of the project is: code that is written, or a place that holds entries. One of "file", "folder". */
export type EntryKind = "file" | "folder";

/** How much a problem matters, as the editor shows it. One of "error", "warning", "info", "hint". */
export type Severity = "error" | "warning" | "info" | "hint";

/** What a line of a command's output is: a problem a tool found, a place it named, a test, or a change. One of "error", "warning", "location", "test_passed", "test_failed", "test_ignored", "change". */
export type ItemKind = "error" | "warning" | "location" | "test_passed" | "test_failed" | "test_ignored" | "change";

/** The arguments of `set_language`. */
export interface SetLanguageArgs {
  language: string;
}

/** The arguments of `replace_text`. */
export interface ReplaceTextArgs {
  text: string;
}

/** The arguments of `replace_range`. */
export interface ReplaceRangeArgs {
  at: number;
  text: string;
  count?: number;
}

/** The arguments of `duplicate_line`. */
export interface DuplicateLineArgs {
  line?: number;
}

/** The arguments of `move_line`. */
export interface MoveLineArgs {
  line?: number;
  up?: boolean;
}

/** The arguments of `delete_line`. */
export interface DeleteLineArgs {
  line?: number;
}

/** The arguments of `toggle_comment`. */
export interface ToggleCommentArgs {
  line?: number;
}

/** The arguments of `indent_line`. */
export interface IndentLineArgs {
  line?: number;
}

/** The arguments of `outdent_line`. */
export interface OutdentLineArgs {
  line?: number;
}

/** The arguments of `reveal_line`. */
export interface RevealLineArgs {
  line?: number;
  column?: number;
}

/** The arguments of `add_child`. */
export interface AddChildArgs {
}

/** The arguments of `rename`. */
export interface RenameArgs {
  entry: Entry;
  name: string;
}

/** The arguments of `move_entry`. */
export interface MoveEntryArgs {
  entry: Entry;
  folder?: Entry;
  before?: Entry;
}

/** The arguments of `move_after`. */
export interface MoveAfterArgs {
  entry: Entry;
  after: Entry;
}

/** The arguments of `delete`. */
export interface DeleteArgs {
  entry: Entry;
}

/** The arguments of `find_in_window`. */
export interface FindInWindowArgs {
  text: string;
  entry?: Entry;
}

/** The arguments of `check`. */
export interface CheckArgs {
  entry: Entry;
}

/** The arguments of `open_folder`. */
export interface OpenFolderArgs {
  folder: string;
}

/** The arguments of `read_into`. */
export interface ReadIntoArgs {
  answer: string;
  parent?: Entry;
}

/** The arguments of `read_folder_into`. */
export interface ReadFolderIntoArgs {
  folder: Entry;
  path: string;
}

/** The arguments of `read_text`. */
export interface ReadTextArgs {
  entry: Entry;
}

/** The arguments of `open_file`. */
export interface OpenFileArgs {
  file: unknown;
}

/** The arguments of `save`. */
export interface SaveArgs {
  entry: Entry;
}

/** The arguments of `overwrite`. */
export interface OverwriteArgs {
  entry: Entry;
}

/** The arguments of `reload`. */
export interface ReloadArgs {
  entry: Entry;
}

/** The arguments of `save_all`. */
export interface SaveAllArgs {
}

/** The arguments of `save_into`. */
export interface SaveIntoArgs {
  entry: Entry;
  files: unknown[];
}

/** The arguments of `move_on_disk`. */
export interface MoveOnDiskArgs {
  entry: Entry;
  to: string;
}

/** The arguments of `import_dropped`. */
export interface ImportDroppedArgs {
  files: unknown[];
}

/** The arguments of `run`. */
export interface RunArgs {
  command: string;
  cwd?: string;
}

/** The arguments of `stop`. */
export interface StopArgs {
}

/** The arguments of `rerun`. */
export interface RerunArgs {
  run: Command;
}

/** The arguments of `rerun_test`. */
export interface RerunTestArgs {
  run: Command;
  test: string;
}

/** The arguments of `run_`. */
export interface ProjectRunArgs {
  cwd: string;
  command: string;
}

/** The arguments of `clear_commands`. */
export interface ClearCommandsArgs {
}

/** The arguments of `open_terminal`. */
export interface OpenTerminalArgs {
}

/** The arguments of `ran`. */
export interface RanArgs {
  j: unknown;
}

/** The arguments of `searched`. */
export interface SearchedArgs {
  j: unknown;
}

/** The arguments of `reveal_item`. */
export interface RevealItemArgs {
  path: string;
  line?: number;
}

/** The arguments of `go_to_line`. */
export interface GoToLineArgs {
  path: string;
  line?: number;
}

/** The arguments of `search_project`. */
export interface SearchProjectArgs {
  needle: string;
  case?: boolean;
  word?: boolean;
}

/** The arguments of `stop_search`. */
export interface StopSearchArgs {
}

/** The arguments of `open_hit`. */
export interface OpenHitArgs {
  path: string;
  line?: number;
}

/** The arguments of `ensure_server`. */
export interface EnsureServerArgs {
  entry: Entry;
}

/** The arguments of `announce`. */
export interface AnnounceArgs {
  entry: Entry;
}

/** The arguments of `ask_server`. */
export interface AskServerArgs {
  entry: Entry;
  what: string;
  line: number;
  column: number;
}

/** The arguments of `complete`. */
export interface CompleteArgs {
  entry: Entry;
  line?: number;
  column?: number;
}

/** The arguments of `hover`. */
export interface HoverArgs {
  entry: Entry;
  line?: number;
  column?: number;
}

/** The arguments of `definition`. */
export interface DefinitionArgs {
  entry: Entry;
  line?: number;
  column?: number;
}

/** The arguments of `accept_completion`. */
export interface AcceptCompletionArgs {
  entry: Entry;
  label: string;
  insert: string;
}

/** The arguments of `format_file`. */
export interface FormatFileArgs {
  entry: Entry;
}

/** The arguments of `toggle_language_servers`. */
export interface ToggleLanguageServersArgs {
  servers?: boolean;
}

/** The arguments of `toggle_format_on_save`. */
export interface ToggleFormatOnSaveArgs {
  format?: boolean;
}

/** The arguments of `restart_server`. */
export interface RestartServerArgs {
}

/** The arguments of `zoom_in`. */
export interface ZoomInArgs {
}

/** The arguments of `zoom_out`. */
export interface ZoomOutArgs {
}

/** The arguments of `zoom_reset`. */
export interface ZoomResetArgs {
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

/** Entry — `tl.so.tilo.code-editor.Entry`. */
export interface Entry extends tilo.Resource {
  jr3jrrmx: EntryKind;  // kind
  wlnikenq: string;  // name
  otwgp44c: Entry | null;  // parent
  fav4hm4j: string;  // language
  zbluopai: string;  // text
  n6s6du45: string;  // disk
  lkyxb2f7: number;  // stamp
  wxaqrw5b: number;  // stamp_len
  bvn4nnf5: string;  // saved
  uhopusbk: boolean;  // stale
  o3gbo56k: Problem[];  // problems
  jwc6eef6: string;  // checked
  jwkhracm: string;  // source
  zu6g3bmd: number;  // reveal_line
  k4etx7ch: number;  // reveal_column
  wtzldj2n: number;  // reveal_ask
  /** Sets the language `entry` is coloured and checked as, by the name of a language the editor knows. */
  set_language(args: {
    language: string;
  }): Promise<void>;
  /** Runs `set_language` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_languageMany(items: SetLanguageArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Writes `text` as the whole file, for an agent or a script that holds a file's new text. */
  replace_text(args: {
    text: string;
  }): Promise<void>;
  /** Runs `replace_text` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  replace_textMany(items: ReplaceTextArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Replaces `count` characters from `at` with `text`: a range or a caret in one file, counted in characters from its start. */
  replace_range(args: {
    at: number;
    text: string;
    count?: number;
  }): Promise<void>;
  /** Runs `replace_range` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  replace_rangeMany(items: ReplaceRangeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Copies a line of the file to the line under it. */
  duplicate_line(args?: {
    line?: number;
  }): Promise<void>;
  /** Runs `duplicate_line` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  duplicate_lineMany(items: DuplicateLineArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves a line over the one above it (`up`) or under the one below it. */
  move_line(args?: {
    line?: number;
    up?: boolean;
  }): Promise<void>;
  /** Runs `move_line` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_lineMany(items: MoveLineArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes a line of the file. */
  delete_line(args?: {
    line?: number;
  }): Promise<void>;
  /** Runs `delete_line` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_lineMany(items: DeleteLineArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Comments out a line, or takes its comment away when the line stands commented already: the marker is the language's own, at the line's own indentation. */
  toggle_comment(args?: {
    line?: number;
  }): Promise<void>;
  /** Runs `toggle_comment` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  toggle_commentMany(items: ToggleCommentArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds one indentation step to a line. */
  indent_line(args?: {
    line?: number;
  }): Promise<void>;
  /** Runs `indent_line` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  indent_lineMany(items: IndentLineArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes one indentation step off a line. */
  outdent_line(args?: {
    line?: number;
  }): Promise<void>;
  /** Runs `outdent_line` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  outdent_lineMany(items: OutdentLineArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Finds the words `text` in a file, the case of their letters aside unless `case` says otherwise; `word` matches only where they stand alone as a word. */
  find_text(args: {
    text: string;
    case?: boolean;
    word?: boolean;
  }): Promise<Found[]>;
  /** Replaces every place the words `text` stand in a file, answering how many changed. */
  replace_in_file(args: {
    text: string;
    with: string;
    case?: boolean;
    word?: boolean;
  }): Promise<number>;
  /** The declarations a file's text writes — `fn`, `struct`, `enum`, `trait`, `class`, `def`, `interface`, `type`, `const` — each with the line and column it stands at: what the project's symbols are and what go-to-symbol offers. */
  symbols(): Promise<Found[]>;
  /** Shows `line` of `entry`, counted from 0: the editor scrolls it into view with the caret on it. */
  reveal_line(args?: {
    line?: number;
    column?: number;
  }): Promise<void>;
  /** Runs `reveal_line` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  reveal_lineMany(items: RevealLineArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Starts a file inside the folder `e`: a row's '+', pressed. */
  add_child(): Promise<void>;
  /** Runs `add_child` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  add_childMany(items: AddChildArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Entry` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ENTRY_ACTIONS = ["set_language", "set_languageMany", "replace_text", "replace_textMany", "replace_range", "replace_rangeMany", "duplicate_line", "duplicate_lineMany", "move_line", "move_lineMany", "delete_line", "delete_lineMany", "toggle_comment", "toggle_commentMany", "indent_line", "indent_lineMany", "outdent_line", "outdent_lineMany", "find_text", "replace_in_file", "symbols", "reveal_line", "reveal_lineMany", "add_child", "add_childMany"] as const;

/** The `tl.so.tilo.code-editor.Entry` kind at `path`. */
export function entry(ws: tilo.Workspace, path: string): Entry {
  return resourceProxy(ws, path, SURFACE, ENTRY_ACTIONS) as unknown as Entry;
}

/** Problem — `tl.so.tilo.code-editor.Problem`. */
export interface Problem extends tilo.Resource {
  nbyucnyk: Severity;  // severity
  cg3ctzdq: number;  // line
  f5kva2nw: number;  // column
  e35xwjly: string;  // message
}

/** The actions `Problem` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PROBLEM_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor.Problem` kind at `path`. */
export function problem(ws: tilo.Workspace, path: string): Problem {
  return resourceProxy(ws, path, SURFACE, PROBLEM_ACTIONS) as unknown as Problem;
}

/** Found — `tl.so.tilo.code-editor.Found`. */
export interface Found extends tilo.Resource {
  hzzanee7: number;  // line
  qt6b4p4a: number;  // column
  "55vdsdve": string;  // words
}

/** The actions `Found` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FOUND_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor.Found` kind at `path`. */
export function found(ws: tilo.Workspace, path: string): Found {
  return resourceProxy(ws, path, SURFACE, FOUND_ACTIONS) as unknown as Found;
}

/** Item — `tl.so.tilo.code-editor.Item`. */
export interface Item extends tilo.Resource {
  gvqsbkfl: ItemKind;  // kind
  "6zcxi5vb": string;  // text
  effdgz7q: string;  // path
  "62tkoolt": number;  // line
  "3ritafbo": number;  // column
}

/** The actions `Item` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ITEM_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor.Item` kind at `path`. */
export function item(ws: tilo.Workspace, path: string): Item {
  return resourceProxy(ws, path, SURFACE, ITEM_ACTIONS) as unknown as Item;
}

/** Command — `tl.so.tilo.code-editor.Command`. */
export interface Command extends tilo.Resource {
  c4ytwucq: string;  // command
  i3avwdem: string;  // cwd
  "2xcd2xlu": string;  // started
  thtznwi2: string;  // ended
  vjb7fevj: number;  // exit
  bukegscc: string;  // output
  xa7laxtf: boolean;  // truncated
  dgchvwcx: string;  // job
  ufboumgh: string;  // state
  cck6yows: Item[];  // items
}

/** The actions `Command` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const COMMAND_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor.Command` kind at `path`. */
export function command(ws: tilo.Workspace, path: string): Command {
  return resourceProxy(ws, path, SURFACE, COMMAND_ACTIONS) as unknown as Command;
}

/** Hit — `tl.so.tilo.code-editor.Hit`. */
export interface Hit extends tilo.Resource {
  bcptqatu: string;  // path
  zwn4bgwt: string;  // rel
  bgpnyksl: number;  // line
  hbulfnxb: number;  // column
  uk7foyau: number;  // len
  hl3ypi66: string;  // preview
}

/** The actions `Hit` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const HIT_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor.Hit` kind at `path`. */
export function hit(ws: tilo.Workspace, path: string): Hit {
  return resourceProxy(ws, path, SURFACE, HIT_ACTIONS) as unknown as Hit;
}

/** Completion — `tl.so.tilo.code-editor.Completion`. */
export interface Completion extends tilo.Resource {
  qg4m5uwr: string;  // label
  xm4myjqj: string;  // detail
  kwj5llbq: string;  // insert
}

/** The actions `Completion` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const COMPLETION_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor.Completion` kind at `path`. */
export function completion(ws: tilo.Workspace, path: string): Completion {
  return resourceProxy(ws, path, SURFACE, COMPLETION_ACTIONS) as unknown as Completion;
}

/** Location — `tl.so.tilo.code-editor.Location`. */
export interface Location extends tilo.Resource {
  trhzxars: string;  // path
  pu4qmppj: number;  // line
  e2bphgrg: number;  // column
}

/** The actions `Location` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LOCATION_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor.Location` kind at `path`. */
export function codeEditorLocation(ws: tilo.Workspace, path: string): Location {
  return resourceProxy(ws, path, SURFACE, LOCATION_ACTIONS) as unknown as Location;
}

/** Loaded — `tl.so.tilo.code-editor.Loaded`. */
export interface Loaded extends tilo.Resource {
  pisboohb: string;  // text
  wcz4xfj7: number;  // mtime
  jqqoxnpy: number;  // len
}

/** The actions `Loaded` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LOADED_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor.Loaded` kind at `path`. */
export function loaded(ws: tilo.Workspace, path: string): Loaded {
  return resourceProxy(ws, path, SURFACE, LOADED_ACTIONS) as unknown as Loaded;
}

/** Stamped — `tl.so.tilo.code-editor.Stamped`. */
export interface Stamped extends tilo.Resource {
  fj7hqru4: number;  // mtime
  qw5jig3a: number;  // len
}

/** The actions `Stamped` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const STAMPED_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor.Stamped` kind at `path`. */
export function stamped(ws: tilo.Workspace, path: string): Stamped {
  return resourceProxy(ws, path, SURFACE, STAMPED_ACTIONS) as unknown as Stamped;
}

/** Exists — `tl.so.tilo.code-editor.Exists`. */
export interface Exists extends tilo.Resource {
  uvwwyzvs: boolean;  // exists
  "7n6rmzco": number;  // mtime
  pwjqsxzz: number;  // len
}

/** The actions `Exists` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const EXISTS_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor.Exists` kind at `path`. */
export function exists(ws: tilo.Workspace, path: string): Exists {
  return resourceProxy(ws, path, SURFACE, EXISTS_ACTIONS) as unknown as Exists;
}

/** DiskAsk — `tl.so.tilo.code-editor.DiskAsk`. */
export interface DiskAsk extends tilo.Resource {
  kn6q47ka: string;  // path
}

/** The actions `DiskAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DISKASK_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor.DiskAsk` kind at `path`. */
export function diskAsk(ws: tilo.Workspace, path: string): DiskAsk {
  return resourceProxy(ws, path, SURFACE, DISKASK_ACTIONS) as unknown as DiskAsk;
}

/** DiskSave — `tl.so.tilo.code-editor.DiskSave`. */
export interface DiskSave extends tilo.Resource {
  haepmky3: string;  // path
  pwykv2y3: string;  // text
  zetzlvco: number;  // mtime
  evmprhon: number;  // len
  om5axsdm: boolean;  // force
}

/** The actions `DiskSave` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DISKSAVE_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor.DiskSave` kind at `path`. */
export function diskSave(ws: tilo.Workspace, path: string): DiskSave {
  return resourceProxy(ws, path, SURFACE, DISKSAVE_ACTIONS) as unknown as DiskSave;
}

/** DiskEntry — `tl.so.tilo.code-editor.DiskEntry`. */
export interface DiskEntry extends tilo.Resource {
  bfnbknrx: string;  // name
  wv4a7ixx: string;  // path
  "7z4nle5w": boolean;  // folder
  pp4ozwjd: boolean;  // hidden
}

/** The actions `DiskEntry` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DISKENTRY_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor.DiskEntry` kind at `path`. */
export function diskEntry(ws: tilo.Workspace, path: string): DiskEntry {
  return resourceProxy(ws, path, SURFACE, DISKENTRY_ACTIONS) as unknown as DiskEntry;
}

/** Listing — `tl.so.tilo.code-editor.Listing`. */
export interface Listing extends tilo.Resource {
  pd7egi2y: DiskEntry[];  // entries
}

/** The actions `Listing` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LISTING_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor.Listing` kind at `path`. */
export function listing(ws: tilo.Workspace, path: string): Listing {
  return resourceProxy(ws, path, SURFACE, LISTING_ACTIONS) as unknown as Listing;
}

/** SearchAsk — `tl.so.tilo.code-editor.SearchAsk`. */
export interface SearchAsk extends tilo.Resource {
  xkrqnga5: string;  // root
  "5dkbrhtv": string;  // needle
  cdk5sbcj: boolean;  // case_sensitive
  b7eojwjq: boolean;  // whole_word
}

/** The actions `SearchAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SEARCHASK_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor.SearchAsk` kind at `path`. */
export function searchAsk(ws: tilo.Workspace, path: string): SearchAsk {
  return resourceProxy(ws, path, SURFACE, SEARCHASK_ACTIONS) as unknown as SearchAsk;
}

/** RunAsk — `tl.so.tilo.code-editor.RunAsk`. */
export interface RunAsk extends tilo.Resource {
  qhqdbt2f: string;  // command
  vqlkdbpf: string;  // cwd
}

/** The actions `RunAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RUNASK_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor.RunAsk` kind at `path`. */
export function runAsk(ws: tilo.Workspace, path: string): RunAsk {
  return resourceProxy(ws, path, SURFACE, RUNASK_ACTIONS) as unknown as RunAsk;
}

/** SearchState — `tl.so.tilo.code-editor.SearchState`. */
export interface SearchState extends tilo.Resource {
  zby3g4ps: string;  // state
  aqlgsja7: Hit[];  // hits
  vp2ht5bq: number;  // files
  izoyjzkp: boolean;  // truncated
}

/** The actions `SearchState` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SEARCHSTATE_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor.SearchState` kind at `path`. */
export function searchState(ws: tilo.Workspace, path: string): SearchState {
  return resourceProxy(ws, path, SURFACE, SEARCHSTATE_ACTIONS) as unknown as SearchState;
}

/** RunState — `tl.so.tilo.code-editor.RunState`. */
export interface RunState extends tilo.Resource {
  coxzozlk: string;  // state
  swqx5bgj: number;  // exit
  wsbgo4u6: string;  // output
  vlearfbg: boolean;  // truncated
  iqbxloe7: string;  // failure
}

/** The actions `RunState` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RUNSTATE_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor.RunState` kind at `path`. */
export function runState(ws: tilo.Workspace, path: string): RunState {
  return resourceProxy(ws, path, SURFACE, RUNSTATE_ACTIONS) as unknown as RunState;
}

/** ServerAsk — `tl.so.tilo.code-editor.ServerAsk`. */
export interface ServerAsk extends tilo.Resource {
  dwxem6ty: string;  // language
  fegqdch5: string;  // root
}

/** The actions `ServerAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SERVERASK_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor.ServerAsk` kind at `path`. */
export function serverAsk(ws: tilo.Workspace, path: string): ServerAsk {
  return resourceProxy(ws, path, SURFACE, SERVERASK_ACTIONS) as unknown as ServerAsk;
}

/** DocumentAsk — `tl.so.tilo.code-editor.DocumentAsk`. */
export interface DocumentAsk extends tilo.Resource {
  h63xj7zy: string;  // op
  cimxyukl: string;  // path
  u2ozcojk: string;  // language
  kxtscxdf: string;  // text
  xbl32ey4: number;  // version
}

/** The actions `DocumentAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DOCUMENTASK_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor.DocumentAsk` kind at `path`. */
export function documentAsk(ws: tilo.Workspace, path: string): DocumentAsk {
  return resourceProxy(ws, path, SURFACE, DOCUMENTASK_ACTIONS) as unknown as DocumentAsk;
}

/** AskAsk — `tl.so.tilo.code-editor.AskAsk`. */
export interface AskAsk extends tilo.Resource {
  vxmeoszq: string;  // op
  ysxutqlg: string;  // path
  p4m2am32: number;  // line
  q3inki66: number;  // column
  l45wtegn: string;  // what
}

/** The actions `AskAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ASKASK_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor.AskAsk` kind at `path`. */
export function askAsk(ws: tilo.Workspace, path: string): AskAsk {
  return resourceProxy(ws, path, SURFACE, ASKASK_ACTIONS) as unknown as AskAsk;
}

/** Progress — `tl.so.tilo.code-editor.Progress`. */
export interface Progress extends tilo.Resource {
  agn2f2db: string;  // title
  g7hzlu2h: string;  // detail
  whcglspc: number;  // percent
}

/** The actions `Progress` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PROGRESS_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor.Progress` kind at `path`. */
export function progress(ws: tilo.Workspace, path: string): Progress {
  return resourceProxy(ws, path, SURFACE, PROGRESS_ACTIONS) as unknown as Progress;
}

/** Diag — `tl.so.tilo.code-editor.Diag`. */
export interface Diag extends tilo.Resource {
  ioyqdqpm: string;  // severity
  beb3ijea: number;  // line
  mev4fmk5: number;  // column
  iwar3uyn: string;  // message
}

/** The actions `Diag` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DIAG_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor.Diag` kind at `path`. */
export function diag(ws: tilo.Workspace, path: string): Diag {
  return resourceProxy(ws, path, SURFACE, DIAG_ACTIONS) as unknown as Diag;
}

/** CompletionAnswer — `tl.so.tilo.code-editor.CompletionAnswer`. */
export interface CompletionAnswer extends tilo.Resource {
  z5gkzdtb: string;  // what
  xm2tpahh: Completion[];  // items
}

/** The actions `CompletionAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const COMPLETIONANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor.CompletionAnswer` kind at `path`. */
export function completionAnswer(ws: tilo.Workspace, path: string): CompletionAnswer {
  return resourceProxy(ws, path, SURFACE, COMPLETIONANSWER_ACTIONS) as unknown as CompletionAnswer;
}

/** HoverAnswer — `tl.so.tilo.code-editor.HoverAnswer`. */
export interface HoverAnswer extends tilo.Resource {
  jchhxxfm: string;  // what
  m4blp7e7: string;  // text
}

/** The actions `HoverAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const HOVERANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor.HoverAnswer` kind at `path`. */
export function hoverAnswer(ws: tilo.Workspace, path: string): HoverAnswer {
  return resourceProxy(ws, path, SURFACE, HOVERANSWER_ACTIONS) as unknown as HoverAnswer;
}

/** DefinitionAnswer — `tl.so.tilo.code-editor.DefinitionAnswer`. */
export interface DefinitionAnswer extends tilo.Resource {
  qb2osbdn: string;  // what
  u57x3dub: Location[];  // locations
}

/** The actions `DefinitionAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DEFINITIONANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor.DefinitionAnswer` kind at `path`. */
export function definitionAnswer(ws: tilo.Workspace, path: string): DefinitionAnswer {
  return resourceProxy(ws, path, SURFACE, DEFINITIONANSWER_ACTIONS) as unknown as DefinitionAnswer;
}

/** FormatEdit — `tl.so.tilo.code-editor.FormatEdit`. */
export interface FormatEdit extends tilo.Resource {
  "7flc4fke": number;  // start
  g2cgs6ok: number;  // start_column
  wfsos5vr: number;  // end
  jhfxuyi3: number;  // end_column
  yj73ybbe: string;  // text
}

/** The actions `FormatEdit` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FORMATEDIT_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor.FormatEdit` kind at `path`. */
export function formatEdit(ws: tilo.Workspace, path: string): FormatEdit {
  return resourceProxy(ws, path, SURFACE, FORMATEDIT_ACTIONS) as unknown as FormatEdit;
}

/** FormatAnswer — `tl.so.tilo.code-editor.FormatAnswer`. */
export interface FormatAnswer extends tilo.Resource {
  um2x3bkg: string;  // what
  wikls7kq: FormatEdit[];  // edits
  zke66gso: string;  // error
}

/** The actions `FormatAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FORMATANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor.FormatAnswer` kind at `path`. */
export function formatAnswer(ws: tilo.Workspace, path: string): FormatAnswer {
  return resourceProxy(ws, path, SURFACE, FORMATANSWER_ACTIONS) as unknown as FormatAnswer;
}

/** ServerProgress — `tl.so.tilo.code-editor.ServerProgress`. */
export interface ServerProgress extends tilo.Resource {
  b22hm43k: string;  // title
  "33vetzuz": string;  // detail
  xvz6ifep: number | null;  // percent
}

/** The actions `ServerProgress` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SERVERPROGRESS_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor.ServerProgress` kind at `path`. */
export function serverProgress(ws: tilo.Workspace, path: string): ServerProgress {
  return resourceProxy(ws, path, SURFACE, SERVERPROGRESS_ACTIONS) as unknown as ServerProgress;
}

/** ServerState — `tl.so.tilo.code-editor.ServerState`. */
export interface ServerState extends tilo.Resource {
  mxvtycpj: string;  // state
  kqcbbx2v: string;  // server
  "66ixtrna": string;  // failure
  sqfl7jdi: ServerProgress | null;  // progress
  s4bc4e6s: Record<string, Diag[]> | null;  // diagnostics
  h4pgltlo: string;  // answer
}

/** The actions `ServerState` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SERVERSTATE_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor.ServerState` kind at `path`. */
export function serverState(ws: tilo.Workspace, path: string): ServerState {
  return resourceProxy(ws, path, SURFACE, SERVERSTATE_ACTIONS) as unknown as ServerState;
}

/** Project — `tl.so.tilo.code-editor.Project`. */
export interface Project extends tilo.Resource {
  evbsknez: Array<tilo.Entry<Entry>>;  // files
  inupl3md: Array<tilo.Entry<Command>>;  // commands
  ubr6i22h: string;  // folder
  "7sbddtl7": string;  // server
  w3s6k4wz: string;  // server_language
  mkyngogi: string;  // server_trouble
  yukh3x5s: boolean;  // language_servers
  "5qk4qm7f": boolean;  // format_on_save
  eolbog7w: string;  // answer_what
  "5vjxpshd": number;  // answer_line
  w3z62her: number;  // answer_column
  t4nipezs: string;  // search_job
  nbqioo6i: string;  // search_query
  "5z5b5fqg": string;  // search_text
  "7tocyk5d": boolean;  // search_case
  t6mgiinw: boolean;  // search_word
  of2jzwnc: string;  // search_state
  nmqjtfyh: Hit[];  // search_hits
  "3gaova7k": number;  // search_files
  w3quhvl4: boolean;  // search_truncated
  b5q44glb: Found[];  // found
  vwrqicgc: string;  // trouble
  qenijz3o: Array<tilo.Entry<Settings>>;  // settings
  /** Starts a file called `name` in `folder`, holding `text`: what a person types in the project, what a drop makes, and where an import lands. */
  new_file(args?: {
    name?: string;
    folder?: Entry;
    text?: string;
  }): Promise<Entry>;
  /** Starts a folder called `name` in `folder`. */
  new_folder(args?: {
    name?: string;
    folder?: Entry;
  }): Promise<Entry>;
  /** Renames `entry` to `name`, keeping the names in its folder apart. */
  rename(args: {
    entry: Entry;
    name: string;
  }): Promise<void>;
  /** Runs `rename` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  renameMany(items: RenameArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves `entry` into `folder` (the top of the project when none), just before `before` — or after every entry there when none names one. */
  move_entry(args: {
    entry: Entry;
    folder?: Entry;
    before?: Entry;
  }): Promise<void>;
  /** Runs `move_entry` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_entryMany(items: MoveEntryArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves `entry` just after `after`, as a drop on the row below asks. */
  move_after(args: {
    entry: Entry;
    after: Entry;
  }): Promise<void>;
  /** Runs `move_after` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_afterMany(items: MoveAfterArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes `entry`, and every entry under it: a folder empties with its entries, as one change a person takes back at once. */
  delete(args: {
    entry: Entry;
  }): Promise<void>;
  /** Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteMany(items: DeleteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The window's search finds the words in the open file and across the project's files: the first place in the file is shown, so a hit is in sight as the words are typed. */
  find_in_window(args: {
    text: string;
    entry?: Entry;
  }): Promise<void>;
  /** Runs `find_in_window` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  find_in_windowMany(items: FindInWindowArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Checks a file and keeps what it found: the device's server's problems when one answers for the file, the editor's own otherwise. */
  check(args: {
    entry: Entry;
  }): Promise<void>;
  /** Runs `check` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  checkMany(items: CheckArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens the folder `folder` as the project: each file and folder it holds, read as entries, and each file's bytes as its text. */
  open_folder(args: {
    folder: string;
  }): Promise<void>;
  /** Runs `open_folder` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  open_folderMany(items: OpenFolderArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Records the entries `answer` lists, standing in `folder`, and reads a file's own bytes: the tree is the project's, so every file the project shows is one a save can check. The listing crosses an action call as the text the device answered, which it already is. */
  read_into(args: {
    answer: string;
    parent?: Entry;
  }): Promise<void>;
  /** Runs `read_into` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  read_intoMany(items: ReadIntoArgs[]): Promise<tilo.ActionAnswer[]>;
  read_folder_into(args: {
    folder: Entry;
    path: string;
  }): Promise<void>;
  /** Runs `read_folder_into` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  read_folder_intoMany(items: ReadFolderIntoArgs[]): Promise<tilo.ActionAnswer[]>;
  read_text(args: {
    entry: Entry;
  }): Promise<void>;
  /** Runs `read_text` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  read_textMany(items: ReadTextArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens a file a person chose, or one Files or another app handed over: `file` is the file it is, and its words become an entry of the project. */
  open_file(args: {
    file: unknown;
  }): Promise<void>;
  /** Runs `open_file` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  open_fileMany(items: OpenFileArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Writes a file's text to the device, checking first that the bytes there are the ones this project read: a file something else changed is not written over without the person saying so. */
  save(args: {
    entry: Entry;
  }): Promise<void>;
  /** Runs `save` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  saveMany(items: SaveArgs[]): Promise<tilo.ActionAnswer[]>;
  write(args: {
    entry: Entry;
    force: boolean;
  }): Promise<boolean>;
  /** Saves a file over the device's change, as a person chose after reading what changed. */
  overwrite(args: {
    entry: Entry;
  }): Promise<void>;
  /** Runs `overwrite` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  overwriteMany(items: OverwriteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reads the file again from the device, dropping what changed here. */
  reload(args: {
    entry: Entry;
  }): Promise<void>;
  /** Runs `reload` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  reloadMany(items: ReloadArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Writes every file that changed here to the device. */
  save_all(): Promise<void>;
  /** Runs `save_all` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  save_allMany(items: SaveAllArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Writes `entry` into the folder `files` names, under its own name. */
  save_into(args: {
    entry: Entry;
    files: unknown[];
  }): Promise<void>;
  /** Runs `save_into` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  save_intoMany(items: SaveIntoArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves a file into the folder `to` of the project, on the device as in the tree. */
  move_on_disk(args: {
    entry: Entry;
    to: string;
  }): Promise<void>;
  /** Runs `move_on_disk` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_on_diskMany(items: MoveOnDiskArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Imports the files a person dropped on the window: a folder becomes a folder of the project, a text file an entry with its words. */
  import_dropped(args: {
    files: unknown[];
  }): Promise<void>;
  /** Runs `import_dropped` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  import_droppedMany(items: ImportDroppedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Runs `command` in the project's folder, as the terminal does: its output and its exit status are kept with the command, and a person reads them beside the code. */
  run(args: {
    command: string;
    cwd?: string;
  }): Promise<void>;
  /** Runs `run` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  runMany(items: RunArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Stops the command that runs in the project now. */
  stop(): Promise<void>;
  /** Runs `stop` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  stopMany(items: StopArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Runs a command of the project's terminal again, as a person does from its history. */
  rerun(args: {
    run: Command;
  }): Promise<void>;
  /** Runs `rerun` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rerunMany(items: RerunArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Runs one test of a run again: the command with the test's name after it, run as that run's tool ran it. */
  rerun_test(args: {
    run: Command;
    test: string;
  }): Promise<void>;
  /** Runs `rerun_test` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rerun_testMany(items: RerunTestArgs[]): Promise<tilo.ActionAnswer[]>;
  run_(args: {
    cwd: string;
    command: string;
  }): Promise<void>;
  /** Runs `run_` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  run_Many(items: ProjectRunArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Erases the terminal's history. */
  clear_commands(): Promise<void>;
  /** Runs `clear_commands` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_commandsMany(items: ClearCommandsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens the project's folder in the system's terminal: the shell a person runs by hand. */
  open_terminal(): Promise<void>;
  /** Runs `open_terminal` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  open_terminalMany(items: OpenTerminalArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The command a job runs ended, or moved: its output, exit status and truncated flag are kept with the command, its output's places become items, and a failure is the window's trouble. */
  ran(args: {
    j: unknown;
  }): Promise<void>;
  /** Runs `ran` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  ranMany(items: RanArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The project search ended, or moved: each hit it found is added as it comes, and the files it searched are counted. */
  searched(args: {
    j: unknown;
  }): Promise<void>;
  /** Runs `searched` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  searchedMany(items: SearchedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Brings the caret to the place an item of a command's output names: the file is read if the project does not hold it yet, and the page shows it. */
  reveal_item(args: {
    path: string;
    line?: number;
  }): Promise<void>;
  /** Runs `reveal_item` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  reveal_itemMany(items: RevealItemArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows `line` of the file at `path`, counted from 0. */
  go_to_line(args: {
    path: string;
    line?: number;
  }): Promise<void>;
  /** Runs `go_to_line` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  go_to_lineMany(items: GoToLineArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Searches every file of the project for `needle`, the case of its letters aside unless `case` says otherwise. */
  search_project(args: {
    needle: string;
    case?: boolean;
    word?: boolean;
  }): Promise<void>;
  /** Runs `search_project` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  search_projectMany(items: SearchProjectArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Stops the project search. */
  stop_search(): Promise<void>;
  /** Runs `stop_search` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  stop_searchMany(items: StopSearchArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows the place a search found: the file is read from the device when the project does not hold it yet, and the page opens it. */
  open_hit(args: {
    path: string;
    line?: number;
  }): Promise<void>;
  /** Runs `open_hit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  open_hitMany(items: OpenHitArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens the language server for a file's language, the project's held connection for that language and folder, or joins the one this device holds already, and tells it about the file. It runs in the app's own process until the project closes it: closing the window ends nothing. */
  ensure_server(args: {
    entry: Entry;
  }): Promise<void>;
  /** Runs `ensure_server` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  ensure_serverMany(items: EnsureServerArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Tells the server that `entry` is open, with the text it holds now, so its answers are about what a person reads. */
  announce(args: {
    entry: Entry;
  }): Promise<void>;
  /** Runs `announce` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  announceMany(items: AnnounceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks the server `what` about a place in a file; its answer arrives as what it says next. */
  ask_server(args: {
    entry: Entry;
    what: string;
    line: number;
    column: number;
  }): Promise<void>;
  /** Runs `ask_server` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  ask_serverMany(items: AskServerArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks the server for completions at a place in a file. */
  complete(args: {
    entry: Entry;
    line?: number;
    column?: number;
  }): Promise<void>;
  /** Runs `complete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  completeMany(items: CompleteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks the server what stands at a place in a file. */
  hover(args: {
    entry: Entry;
    line?: number;
    column?: number;
  }): Promise<void>;
  /** Runs `hover` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  hoverMany(items: HoverArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks the server where the declaration at a place is written. */
  definition(args: {
    entry: Entry;
    line?: number;
    column?: number;
  }): Promise<void>;
  /** Runs `definition` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  definitionMany(items: DefinitionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Writes the completion a person chose where the caret stands. */
  accept_completion(args: {
    entry: Entry;
    label: string;
    insert: string;
  }): Promise<void>;
  /** Runs `accept_completion` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  accept_completionMany(items: AcceptCompletionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reformats a file as its language's formatter does, as one change. */
  format_file(args: {
    entry: Entry;
  }): Promise<void>;
  /** Runs `format_file` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  format_fileMany(items: FormatFileArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Turns language servers on or off, for the project. */
  toggle_language_servers(args?: {
    servers?: boolean;
  }): Promise<void>;
  /** Runs `toggle_language_servers` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  toggle_language_serversMany(items: ToggleLanguageServersArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Turns formatting a file on save on or off. */
  toggle_format_on_save(args?: {
    format?: boolean;
  }): Promise<void>;
  /** Runs `toggle_format_on_save` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  toggle_format_on_saveMany(items: ToggleFormatOnSaveArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Ends the language server, as a person pressing 'Try again' after a failure does: the next file checked starts it again. */
  restart_server(): Promise<void>;
  /** Runs `restart_server` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  restart_serverMany(items: RestartServerArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows the file's problems under the code, as the window's own key does anywhere in it. */
  show_problems(): Promise<void>;
  /** Shows the project's terminal under the code, as Ctrl+J does anywhere in the window. */
  show_terminal(): Promise<void>;
  /** Shows what the project search found under the code, as Ctrl+Shift+F does anywhere in the window. */
  show_search_results(): Promise<void>;
  /** This person's settings, made by their own first change so they are theirs to read and write. */
  ensure_settings(): Promise<Settings>;
  /** Shows the code one step larger: the editor's own text size, kept for the person on every device. */
  zoom_in(): Promise<void>;
  /** Runs `zoom_in` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  zoom_inMany(items: ZoomInArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows the code one step smaller. */
  zoom_out(): Promise<void>;
  /** Runs `zoom_out` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  zoom_outMany(items: ZoomOutArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows the code at its own size again. */
  zoom_reset(): Promise<void>;
  /** Runs `zoom_reset` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  zoom_resetMany(items: ZoomResetArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Project` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PROJECT_ACTIONS = ["new_file", "new_folder", "rename", "renameMany", "move_entry", "move_entryMany", "move_after", "move_afterMany", "delete", "deleteMany", "find_in_window", "find_in_windowMany", "check", "checkMany", "open_folder", "open_folderMany", "read_into", "read_intoMany", "read_folder_into", "read_folder_intoMany", "read_text", "read_textMany", "open_file", "open_fileMany", "save", "saveMany", "write", "overwrite", "overwriteMany", "reload", "reloadMany", "save_all", "save_allMany", "save_into", "save_intoMany", "move_on_disk", "move_on_diskMany", "import_dropped", "import_droppedMany", "run", "runMany", "stop", "stopMany", "rerun", "rerunMany", "rerun_test", "rerun_testMany", "run_", "run_Many", "clear_commands", "clear_commandsMany", "open_terminal", "open_terminalMany", "ran", "ranMany", "searched", "searchedMany", "reveal_item", "reveal_itemMany", "go_to_line", "go_to_lineMany", "search_project", "search_projectMany", "stop_search", "stop_searchMany", "open_hit", "open_hitMany", "ensure_server", "ensure_serverMany", "announce", "announceMany", "ask_server", "ask_serverMany", "complete", "completeMany", "hover", "hoverMany", "definition", "definitionMany", "accept_completion", "accept_completionMany", "format_file", "format_fileMany", "toggle_language_servers", "toggle_language_serversMany", "toggle_format_on_save", "toggle_format_on_saveMany", "restart_server", "restart_serverMany", "show_problems", "show_terminal", "show_search_results", "ensure_settings", "zoom_in", "zoom_inMany", "zoom_out", "zoom_outMany", "zoom_reset", "zoom_resetMany"] as const;

/** The `tl.so.tilo.code-editor.Project` kind at `path`. */
export function project(ws: tilo.Workspace, path: string): Project {
  return resourceProxy(ws, path, SURFACE, PROJECT_ACTIONS) as unknown as Project;
}

/** Settings — `tl.so.tilo.code-editor.Settings`. */
export interface Settings extends tilo.Resource {
  douj4xux: number;  // editor_zoom
}

/** The actions `Settings` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SETTINGS_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor.Settings` kind at `path`. */
export function settings(ws: tilo.Workspace, path: string): Settings {
  return resourceProxy(ws, path, SURFACE, SETTINGS_ACTIONS) as unknown as Settings;
}

/** Code Editor — `tl.so.tilo.code-editor`. */
export interface CodeEditor extends tilo.Resource {
}

/** The actions `CodeEditor` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CODEEDITOR_ACTIONS = [] as const;

/** The `tl.so.tilo.code-editor` kind at `path`. */
export function codeEditor(ws: tilo.Workspace, path: string): CodeEditor {
  return resourceProxy(ws, path, SURFACE, CODEEDITOR_ACTIONS) as unknown as CodeEditor;
}

